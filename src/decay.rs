//! Time-decay / recency weighting for the share ratio (v1 height-rate proxy).
//!
//! `/history` is **cumulative only** — it has no rolling window — so the SDK cannot
//! reconstruct true torrent-style time decay ("ratio over the last 30 days"). v1 ships a
//! **height-rate proxy** that rewards *recent activity* using only `last_height`, and v2
//! (a deferred, observational node change) would add a real window. See
//! `PLAN/02-ratio-economy.md` §3.3 and §7.
//!
//! CRITICAL HONESTY CONSTRAINT: the recency factor is applied **only** to the displayed /
//! soft-gating score. It can *de-prioritize* a dormant high-ratio node but it can never
//! *fabricate* contribution: hard threshold comparisons in [`crate::Ratio`] always use the
//! exact cumulative `contributed/consumed` the chain actually attests, with no decay applied.

/// A recency multiplier in basis points (10_000 = 1.0x, no decay).
///
/// Returned as integer bps to keep the math float-free on the gating path. A caller that wants
/// a display float can divide by 10_000.0, but gating must never branch on a float.
pub const RECENCY_FULL_BPS: u32 = 10_000;

/// Compute a recency multiplier (in bps) for a node given the current beacon height.
///
/// - If the node was active within `active_blocks` of the tip, the factor is full (`10_000`).
/// - Otherwise it decays **linearly** from full down to `floor_bps` over `decay_blocks`
///   of additional dormancy, then stays at `floor_bps` (never zero — a dormant contributor
///   is de-prioritized, not erased).
///
/// `last_height == 0` (a node that has never interacted) returns `floor_bps`: it has no recency
/// to credit. All arithmetic is saturating integer; no panics, no floats.
pub fn recency_factor_bps(
    last_height: u64,
    beacon_height: u64,
    active_blocks: u64,
    decay_blocks: u64,
    floor_bps: u32,
) -> u32 {
    if last_height == 0 {
        return floor_bps;
    }
    // How far behind the tip is this node's last activity?
    let behind = beacon_height.saturating_sub(last_height);
    if behind <= active_blocks {
        return RECENCY_FULL_BPS;
    }
    if decay_blocks == 0 {
        return floor_bps;
    }
    // Linear decay over the dormancy span past `active_blocks`.
    let into_decay = behind - active_blocks;
    if into_decay >= decay_blocks {
        return floor_bps;
    }
    // factor = FULL - (FULL - floor) * into_decay / decay_blocks   (all integer, saturating)
    let span = RECENCY_FULL_BPS.saturating_sub(floor_bps) as u128;
    let drop = span.saturating_mul(into_decay as u128) / (decay_blocks as u128);
    RECENCY_FULL_BPS.saturating_sub(drop as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_node_gets_full_factor() {
        assert_eq!(recency_factor_bps(1000, 1010, 100, 1000, 2000), RECENCY_FULL_BPS);
        // exactly at the edge of the active window
        assert_eq!(recency_factor_bps(900, 1000, 100, 1000, 2000), RECENCY_FULL_BPS);
    }

    #[test]
    fn dormant_node_decays_then_floors() {
        // 600 blocks behind: 100 active + 500 into a 1000-block decay span => half the drop.
        let f = recency_factor_bps(0u64.wrapping_add(1), 601, 100, 1000, 2000);
        // span = 8000, half => drop 4000 => 6000
        assert_eq!(f, 6000);
        // way past the decay span => floor
        assert_eq!(recency_factor_bps(1, 50_000, 100, 1000, 2000), 2000);
    }

    #[test]
    fn never_interacted_returns_floor() {
        assert_eq!(recency_factor_bps(0, 1000, 100, 1000, 2000), 2000);
    }

    #[test]
    fn decay_is_monotonic_non_increasing() {
        let mut prev = RECENCY_FULL_BPS;
        for tip in (100u64..3000).step_by(37) {
            let f = recency_factor_bps(100, tip, 100, 1000, 2000);
            assert!(f <= prev, "recency must not increase as dormancy grows");
            prev = f;
        }
    }
}
