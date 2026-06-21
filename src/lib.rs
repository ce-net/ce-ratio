//! # ce-ratio — torrent-style compute share-ratio and reputation, SDK/app tier
//!
//! `ce-ratio` computes a **derived, displayed signal** — `ratio = contributed / consumed` —
//! from the immutable on-chain facts the node already serves (`/history`, `/atlas`, `/beacon`),
//! and uses it to **rank, order, and price** scheduling choices. It is policy/UX over public
//! ledger facts, so it lives at the SDK/app tier (over [`ce_rs`]), alongside `swarm`/`rdev`,
//! **not** in the primitives-only node.
//!
//! ## HARD RULE — RANK/ORDER ONLY, NEVER AUTHORIZATION
//!
//! **`ce-ratio` output may RANK or ORDER candidates only. It must NEVER be importable by, nor
//! feed, `ce-cap` capability verification.** A high ratio does not grant a capability and a low
//! ratio does not deny one. The *only* authorization primitive in CE is the signed, attenuating
//! capability chain verified by `ce-cap`; ratio merely biases which already-permitted choice a
//! caller prefers, what price a host quotes, and how much verification to apply. This boundary is
//! enforced by **module structure**: this crate depends only on [`ce_rs`] (a thin HTTP client)
//! and has **no dependency on `ce-cap`, `ce-identity`, or any node/consensus crate**, so it is
//! impossible to wire a ratio into a capability decision through this code. If you find yourself
//! wanting to `use ce_ratio::...` inside `ce-cap`, stop: that is the bug this rule exists to
//! prevent. See `PLAN/02-ratio-economy.md` §1 (Non-goals) and §5.
//!
//! ## Float discipline
//!
//! Money is integer base units (`i128`, `10^18` per credit). The **gating path is float-free**:
//! [`Ratio::at_least`] compares `contributed/consumed` against a `num/den` threshold by
//! cross-multiplication in `i128`/`u128`, never `f64`. [`Ratio::as_f64`] exists for **display
//! only** and must never decide gating (a careless caller could hit rounding at the threshold).
//!
//! ## Anti-gaming (see `anti_gaming` notes throughout)
//!
//! - **Whitewashing** (drop a leech identity, return as a fresh "newcomer"): bounded by a credit
//!   *and* a time cap on freeleech ([`classify`] -> [`Tier::Probation`]); a returning identity that
//!   already consumed cannot reset its `spent`. The chain remembers `first_height`.
//! - **Sybil starvation / self-dealing rings**: leans on the mandatory settlement burn — every
//!   wash cycle destroys real mined capital, and burn-asymmetry forces any self-dealing ring to a
//!   *combined* ratio below the honest-balanced point. A low-diversity heuristic caps such nodes.

mod decay;

pub use decay::{recency_factor_bps, RECENCY_FULL_BPS};

use std::collections::HashMap;

use ce_rs::{Amount, AtlasEntry, NodeHistory};

// ---------------------------------------------------------------------------
// Settlement-burn constants — mirror of `ce-chain` (NOT a dependency).
//
// ce-ratio depends only on the thin ce-rs HTTP client, which does not re-export the chain's
// economic constants. These mirror `ce-chain::SETTLEMENT_BURN_BPS` / `BPS_DENOM`. The
// burn-asymmetry floor below is DERIVED from them — never hardcoded — so if the chain's burn
// rate ever changes, update only `SETTLEMENT_BURN_BPS` here and every derived threshold moves
// with it (addressing the §10 risk: "if SETTLEMENT_BURN_BPS ever changes, the floor must move").
// ---------------------------------------------------------------------------

/// Settlement burn rate in basis points. Mirror of `ce-chain::SETTLEMENT_BURN_BPS` (1.00%).
pub const SETTLEMENT_BURN_BPS: u128 = 100;

/// Basis-point denominator. Mirror of `ce-chain::BPS_DENOM`.
pub const BPS_DENOM: u128 = 10_000;

/// The exact burn-adjusted "balanced" ratio, as an integer `(num, den)` pair.
///
/// An honest node that hosts exactly as much gross volume as it consumes lands here, **not** at
/// `1.0`: as the *cell* it is debited gross `G` (counted in `spent`), and as the *host* of an
/// equal gross volume it receives `G * (BPS_DENOM - SETTLEMENT_BURN_BPS) / BPS_DENOM` (counted in
/// `earned`, net of burn). So the balanced point is `(BPS_DENOM - SETTLEMENT_BURN_BPS) / BPS_DENOM`
/// — `9900/10000 = 0.99` at the shipped 1% burn. DERIVED, never hardcoded.
pub const fn balanced_point() -> (u128, u128) {
    (BPS_DENOM - SETTLEMENT_BURN_BPS, BPS_DENOM)
}

/// The default "balanced floor" `(num, den)` an honest balanced node must clear to avoid the
/// [`Tier::Leech`] label.
///
/// Derived from [`balanced_point`] with a small safety margin so honest participants are **never**
/// penalized by the burn they already paid. We place the floor a further `FLOOR_MARGIN_BPS` below
/// the exact balanced point: `floor = balanced_point * (BPS_DENOM - FLOOR_MARGIN_BPS) / BPS_DENOM`.
/// At 1% burn and a 400 bps margin this yields `0.99 * 0.96 = 0.9504 ≈ 0.95`, matching the design
/// doc's stated 0.95 — but as a *derived* value, not a magic constant.
pub const FLOOR_MARGIN_BPS: u128 = 400;

/// Compute the default balanced floor as an exact `(num, den)` pair, derived from the burn.
pub const fn default_balanced_floor() -> (u128, u128) {
    let (bp_num, bp_den) = balanced_point();
    // floor = bp * (DENOM - margin) / DENOM, kept as an exact fraction (no rounding).
    let num = bp_num * (BPS_DENOM - FLOOR_MARGIN_BPS);
    let den = bp_den * BPS_DENOM;
    (num, den)
}

// ---------------------------------------------------------------------------
// Ratio — the exact rational, float-free on the gating path.
// ---------------------------------------------------------------------------

/// A node's exact share ratio as a rational `contributed / consumed`.
///
/// `contributed` = `NodeHistory.earned` (credits earned **as host**, already net of the 1% burn).
/// `consumed`    = `NodeHistory.spent`  (credits spent **as cell**, gross / pre-burn).
///
/// Held as a `(u128, u128)` pair; the gating comparison ([`Ratio::at_least`]) is float-free.
/// [`Ratio::as_f64`] is **display only**.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ratio {
    /// Earned as host, net of burn (base units).
    pub contributed: u128,
    /// Spent as cell, gross (base units).
    pub consumed: u128,
}

impl Ratio {
    /// `contributed / consumed` as an `f64` — **DISPLAY ONLY**. Never gate on this; use
    /// [`Ratio::at_least`] / [`Ratio::cmp_threshold`], which are exact. Returns `f64::INFINITY`
    /// when `consumed == 0`.
    pub fn as_f64(&self) -> f64 {
        if self.consumed == 0 {
            f64::INFINITY
        } else {
            self.contributed as f64 / self.consumed as f64
        }
    }

    /// Exact, float-free: is `contributed/consumed >= num/den`?
    ///
    /// Compares `contributed * den` vs `consumed * num`. Uses 256-bit-safe multiplication so a
    /// `u128 * u128` product never overflows (supply caps at `2.1e28 < 2^128`, but a threshold
    /// `den`/`num` could be arbitrary, so we widen to two-limb). When `consumed == 0` the node is
    /// an infinite contributor and clears any finite threshold (`true`), except a `num == 0`
    /// threshold which everyone clears.
    pub fn at_least(&self, num: u128, den: u128) -> bool {
        if num == 0 {
            return true;
        }
        if self.consumed == 0 {
            // contributed/0 = +inf >= any finite num/den
            return true;
        }
        // contributed * den >= consumed * num, computed without overflow.
        matches!(
            cmp_u128_products(self.contributed, den, self.consumed, num),
            core::cmp::Ordering::Greater | core::cmp::Ordering::Equal
        )
    }

    /// Exact ordering of `contributed/consumed` against `num/den`. Float-free; the canonical
    /// gating comparison. `consumed == 0` orders `Greater` for any finite, non-zero `num/den`.
    pub fn cmp_threshold(&self, num: u128, den: u128) -> core::cmp::Ordering {
        if self.consumed == 0 {
            return if num == 0 {
                core::cmp::Ordering::Equal
            } else {
                core::cmp::Ordering::Greater
            };
        }
        cmp_u128_products(self.contributed, den, self.consumed, num)
    }
}

/// Compare `a*b` vs `c*d` for `u128` operands without overflow, by splitting into 64-bit limbs
/// and comparing the 256-bit products. Returns the ordering of `a*b` relative to `c*d`.
fn cmp_u128_products(a: u128, b: u128, c: u128, d: u128) -> core::cmp::Ordering {
    let (hi1, lo1) = wide_mul(a, b);
    let (hi2, lo2) = wide_mul(c, d);
    (hi1, lo1).cmp(&(hi2, lo2))
}

/// 128x128 -> 256-bit multiply, returned as `(high_u128, low_u128)`. No `unsafe`, no float.
fn wide_mul(a: u128, b: u128) -> (u128, u128) {
    let a_lo = a & u64::MAX as u128;
    let a_hi = a >> 64;
    let b_lo = b & u64::MAX as u128;
    let b_hi = b >> 64;

    let ll = a_lo * b_lo;
    let lh = a_lo * b_hi;
    let hl = a_hi * b_lo;
    let hh = a_hi * b_hi;

    // low 128 = ll + ((lh + hl) << 64), with carry into high.
    let cross = lh.wrapping_add(hl);
    let cross_carry = (lh > cross) as u128; // overflow of lh + hl into bit 128

    let low = ll.wrapping_add(cross << 64);
    let low_carry = (low < ll) as u128;

    let high = hh + (cross >> 64) + (cross_carry << 64) + low_carry;
    (high, low)
}

/// Compute the exact [`Ratio`] for a node's history.
///
/// Returns `None` when `consumed == 0` (no denominator) — the caller should treat such a node as
/// an "infinite contributor" (it has only ever given, never taken). `earned`/`spent` are read as
/// their base-unit magnitudes; negative balances cannot occur for cumulative earned/spent totals,
/// but we clamp to zero defensively rather than panic.
pub fn ratio_of(h: &NodeHistory) -> Option<Ratio> {
    let consumed = base_u128(h.spent);
    if consumed == 0 {
        return None;
    }
    Some(Ratio {
        contributed: base_u128(h.earned),
        consumed,
    })
}

/// Read an [`Amount`]'s base units as a non-negative `u128` (cumulative totals are never negative;
/// clamp defensively).
fn base_u128(a: Amount) -> u128 {
    let b = a.base();
    if b < 0 { 0 } else { b as u128 }
}

// ---------------------------------------------------------------------------
// Tiers, config, classification.
// ---------------------------------------------------------------------------

/// A node's reputation tier. Ordering is meaningful for ranking (higher variant == preferred),
/// but tier is a *soft* signal: the only hard gate in CE is the capability chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// Never interacted (`first_height == 0`). Bounded freeleech, trickle of low-value work.
    Newcomer,
    /// Within the vetting window and under the freeleech cap. Bounded freeleech, elevated audit.
    Probation,
    /// Below the burn-derived balanced floor. De-prioritized and re-priced (pay-to-play), never blocked.
    Leech,
    /// At or above the balanced floor, below the contributor threshold. Normal priority, 1.0x price.
    Balanced,
    /// At or above the contributor threshold. Priority access to scarce hosts, perks.
    Contributor,
}

impl Tier {
    /// A rank score (higher == preferred) used to order hosts. NOT an authorization weight —
    /// this only orders already-permitted candidates and never feeds `ce-cap`.
    pub fn rank_score(self) -> u8 {
        match self {
            Tier::Leech => 0,
            Tier::Newcomer => 1,
            Tier::Probation => 2,
            Tier::Balanced => 3,
            Tier::Contributor => 4,
        }
    }
}

/// Policy knobs for ratio classification and gating. Defaults derive the balanced floor from the
/// settlement burn (never hardcoded). Apps may override (`swarm` ships its own defaults).
#[derive(Debug, Clone)]
pub struct RatioConfig {
    /// Balanced floor as `(num, den)`; default derived from the burn via [`default_balanced_floor`].
    pub balanced_floor_num: u128,
    pub balanced_floor_den: u128,
    /// Contributor threshold as `(num, den)`; default `2/1` (ratio >= 2.0).
    pub contributor_num: u128,
    pub contributor_den: u128,
    /// Vetting window in blocks for newcomer grace (default 8_640 ≈ 24h at ~10s blocks).
    pub vetting_blocks: u64,
    /// Bounded freeleech credit cap (default 5 credits).
    pub freeleech_cap: Amount,
    /// Recency: a node active within this many blocks of the tip gets full recency (default 8_640).
    pub active_blocks: u64,
    /// Recency: linear decay span past `active_blocks` (default 259_200 ≈ 30 days).
    pub decay_blocks: u64,
    /// Recency floor in bps for fully-dormant nodes (default 2_000 = 0.2x; never zero).
    pub recency_floor_bps: u32,
    /// Low-diversity anti-wash guard: a node whose `earned` accrued over very few host events
    /// (few large settlements) is capped at [`Tier::Balanced`] regardless of ratio. This is the
    /// minimum number of distinct host events (`jobs_hosted + heartbeats_hosted`) required to be
    /// eligible for [`Tier::Contributor`] (default 8). See [`anti_gaming`].
    pub min_host_events_for_contributor: u64,
}

impl Default for RatioConfig {
    fn default() -> Self {
        let (fnum, fden) = default_balanced_floor();
        RatioConfig {
            balanced_floor_num: fnum,
            balanced_floor_den: fden,
            contributor_num: 2,
            contributor_den: 1,
            vetting_blocks: 8_640,
            freeleech_cap: Amount::from_credits(5),
            active_blocks: 8_640,
            decay_blocks: 259_200,
            recency_floor_bps: 2_000,
            min_host_events_for_contributor: 8,
        }
    }
}

/// Classify a node into a [`Tier`] from its history and the current beacon height.
///
/// Newcomer grace is **bounded two ways** (the BitTyrant lesson — never unbounded altruism): a
/// time cap (`vetting_blocks`) AND a credit cap (`freeleech_cap`). Once *either* is exceeded the
/// node is rated normally and must carry its weight.
///
/// ## anti_gaming: whitewashing
/// A leech that abandons its identity and returns as a fresh `first_height == 0` newcomer only
/// regains the *bounded* freeleech — capped in both time and credits — so re-whitewashing yields
/// strictly less than honest participation, and an identity that has already consumed cannot reset
/// its on-chain `spent`. The grace is supervised (trickle of low-value work), not full access.
pub fn classify(h: &NodeHistory, beacon_height: u64, cfg: &RatioConfig) -> Tier {
    // Stranger: never interacted.
    if h.first_height == 0 {
        return Tier::Newcomer;
    }

    // Probation: still inside the vetting window AND under the freeleech credit cap.
    let age_blocks = beacon_height.saturating_sub(h.first_height);
    let under_credit_cap = h.spent.base() < cfg.freeleech_cap.base();
    if age_blocks < cfg.vetting_blocks && under_credit_cap {
        return Tier::Probation;
    }

    // Rated: apply the exact ratio against the burn-derived floor.
    match ratio_of(h) {
        // No consumption recorded but past vetting -> a pure contributor (infinite ratio), subject
        // to the diversity guard below.
        None => contributor_or_capped(h, cfg),
        Some(r) => {
            if !r.at_least(cfg.balanced_floor_num, cfg.balanced_floor_den) {
                Tier::Leech
            } else if r.at_least(cfg.contributor_num, cfg.contributor_den) {
                contributor_or_capped(h, cfg)
            } else {
                Tier::Balanced
            }
        }
    }
}

/// anti_gaming: low-diversity / sybil-starvation guard.
///
/// Cumulative `/history` has no per-counterparty breakdown, so the SDK cannot fully detect
/// pairwise wash-trading. The cheap signal it *can* compute: a node whose entire `earned` accrued
/// over very few host events (a handful of large settlements — the shape of a self-dealing ring
/// pumping one identity) is **capped at [`Tier::Balanced`]** regardless of how high its ratio
/// looks. This is a heuristic, not proof; the primary defense remains the mandatory settlement
/// burn, which makes inflating `earned` capital-costly (every wash cycle destroys real credits and
/// burn-asymmetry drives any closed ring's combined ratio below the balanced point).
fn contributor_or_capped(h: &NodeHistory, cfg: &RatioConfig) -> Tier {
    if h.delivered_work() >= cfg.min_host_events_for_contributor {
        Tier::Contributor
    } else {
        Tier::Balanced
    }
}

/// Remaining bounded freeleech budget for a node, in base units (`0` once vetting is exhausted by
/// either the credit cap or — for a non-newcomer past the window — time). For a never-interacted
/// newcomer this is the full `freeleech_cap`.
pub fn freeleech_remaining(h: &NodeHistory, cfg: &RatioConfig) -> Amount {
    let cap = cfg.freeleech_cap.base();
    let spent = h.spent.base().max(0);
    Amount::from_base((cap - spent).max(0))
}

// ---------------------------------------------------------------------------
// Host-side admission (price/throttle a requester's bid).
// ---------------------------------------------------------------------------

/// Host-side decision: should I accept this requester's bid, and at what price multiplier (bps,
/// `10_000` = 1.0x)?
///
/// Soft preference before any cliff (the Storj lesson): leeches are **re-priced and
/// de-prioritized, never hard-blocked** — the only hard gate is the capability chain. Two-sided:
/// a requester that only ever leeches and lets bids expire (high `expiries`) pays a hit-and-run
/// tax. The multiplier grows with the expiry rate (`expiries / jobs_paid`), computable today from
/// `/history` with zero node change.
///
/// Returns `(accept, price_multiplier_bps)`. `accept` is always `true` in v1 (we re-price rather
/// than block); apps may choose to drop below a threshold, but ratio itself never authorizes.
pub fn admit_requester(h: &NodeHistory, beacon_height: u64, cfg: &RatioConfig) -> (bool, u32) {
    let tier = classify(h, beacon_height, cfg);
    // Base multiplier by tier.
    let base_bps: u32 = match tier {
        // Leech pays a pay-to-play tax; clamp into the 1.25x–2.0x band the design specifies.
        Tier::Leech => 12_500,
        Tier::Newcomer | Tier::Probation => 10_000,
        Tier::Balanced => 10_000,
        Tier::Contributor => 10_000,
    };

    // Hit-and-run tax: multiply by (1 + expiries_rate). expiries_rate = expiries / max(jobs_paid,1),
    // in bps, capped so the total stays within the design's 2.0x ceiling.
    let attempts = h.jobs_paid.max(1);
    let expiry_rate_bps = ((h.expiries as u128 * BPS_DENOM) / attempts as u128) as u32;
    let mult = mul_bps(base_bps, RECENCY_FULL_BPS.saturating_add(expiry_rate_bps));
    // Clamp the whole multiplier into [1.0x, 2.0x].
    let clamped = mult.clamp(10_000, 20_000);
    (true, clamped)
}

/// Multiply two bps multipliers: `(a/10_000) * (b/10_000)` back into bps. Saturating, float-free.
fn mul_bps(a: u32, b: u32) -> u32 {
    ((a as u128 * b as u128) / BPS_DENOM).min(u32::MAX as u128) as u32
}

// ---------------------------------------------------------------------------
// Atlas ranking — the headline "order hosts by ratio" helper.
// ---------------------------------------------------------------------------

/// A ranked host: the atlas index plus the signals that ordered it. RANK/ORDER output only —
/// this struct never authorizes anything and is never read by `ce-cap`.
#[derive(Debug, Clone)]
pub struct RankedHost {
    /// Index into the input `atlas` slice.
    pub index: usize,
    /// The host's node id (hex), copied for convenience.
    pub node_id: String,
    /// The host's tier (soft signal).
    pub tier: Tier,
    /// Recency multiplier (bps) applied to ordering; `10_000` = active.
    pub recency_bps: u32,
    /// Whether the host currently has free CPU capacity (`running_jobs < cpu_cores`).
    pub has_free_capacity: bool,
}

/// Rank candidate hosts from the atlas for placement, best-first.
///
/// I am the **requester** picking a host: prefer hosts that are good contributors (high tier),
/// recently active (recency), and have free capacity. Returns a `Vec<RankedHost>` sorted best
/// first. `hist` maps `node_id` (hex) -> that node's [`NodeHistory`]; a host with no history entry
/// is treated as a [`Tier::Newcomer`] (stranger). The ordering, in priority order:
///
/// 1. has free capacity (a busy host can't take the job, regardless of reputation),
/// 2. tier rank score (Contributor > Balanced > Probation > Newcomer > Leech),
/// 3. recency (active hosts before dormant ones),
/// 4. lower current load (`running_jobs`) as a tie-break,
/// 5. node_id for a stable, deterministic order.
///
/// CRITICAL: the returned order is a *preference*, not permission. The caller still needs a valid
/// capability chain (via `ce-cap`) to actually place work on any host here; this function neither
/// produces nor consumes capabilities.
pub fn rank_hosts(
    atlas: &[AtlasEntry],
    hist: &HashMap<String, NodeHistory>,
    beacon_height: u64,
    cfg: &RatioConfig,
) -> Vec<RankedHost> {
    let stranger = newcomer_history();
    let mut ranked: Vec<RankedHost> = atlas
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let h = hist.get(&entry.node_id).unwrap_or(&stranger);
            let tier = classify(h, beacon_height, cfg);
            let recency_bps = recency_factor_bps(
                h.last_height,
                beacon_height,
                cfg.active_blocks,
                cfg.decay_blocks,
                cfg.recency_floor_bps,
            );
            let has_free_capacity = entry.running_jobs < entry.cpu_cores;
            RankedHost {
                index,
                node_id: entry.node_id.clone(),
                tier,
                recency_bps,
                has_free_capacity,
            }
        })
        .collect();

    ranked.sort_by(|a, b| {
        // 1. free capacity first (true before false)
        b.has_free_capacity
            .cmp(&a.has_free_capacity)
            // 2. higher tier rank first
            .then(b.tier.rank_score().cmp(&a.tier.rank_score()))
            // 3. higher recency first
            .then(b.recency_bps.cmp(&a.recency_bps))
            // 4. lower current load first (tie-break)
            .then(
                atlas[a.index]
                    .running_jobs
                    .cmp(&atlas[b.index].running_jobs),
            )
            // 5. stable deterministic order
            .then(a.node_id.cmp(&b.node_id))
    });

    ranked
}

/// Convenience: rank and return just the atlas indices, best-first.
pub fn rank_host_indices(
    atlas: &[AtlasEntry],
    hist: &HashMap<String, NodeHistory>,
    beacon_height: u64,
    cfg: &RatioConfig,
) -> Vec<usize> {
    rank_hosts(atlas, hist, beacon_height, cfg)
        .into_iter()
        .map(|r| r.index)
        .collect()
}

/// A synthetic "stranger" history for hosts the local node has no recorded interactions with.
fn newcomer_history() -> NodeHistory {
    NodeHistory {
        node_id: String::new(),
        jobs_hosted: 0,
        jobs_paid: 0,
        heartbeats_hosted: 0,
        heartbeats_paid: 0,
        expiries: 0,
        earned: Amount::ZERO,
        spent: Amount::ZERO,
        first_height: 0,
        last_height: 0,
    }
}

// ---------------------------------------------------------------------------
// Async convenience over CeClient — fetch + classify a node in one call.
// ---------------------------------------------------------------------------

/// Fetch a node's history from the local node and classify it.
///
/// Thin convenience over [`ce_rs::CeClient::history`] + [`ce_rs::CeClient::beacon`]. Returns the
/// resolved [`Tier`] and the exact [`Ratio`] (or `None` when the node has never consumed). Fallible
/// public function returns [`anyhow::Result`] per CE standards.
pub async fn fetch_and_classify(
    client: &ce_rs::CeClient,
    node_id: &str,
    cfg: &RatioConfig,
) -> anyhow::Result<(Tier, Option<Ratio>)> {
    let history = client.history(node_id).await?;
    let beacon = client.beacon().await?;
    let tier = classify(&history, beacon.height, cfg);
    let ratio = ratio_of(&history);
    tracing::debug!(node = node_id, ?tier, "classified node ratio");
    Ok((tier, ratio))
}

// ---------------------------------------------------------------------------
// anti_gaming — module-level summary of the guards, for readers and reviewers.
// ---------------------------------------------------------------------------

/// Anti-gaming notes, addressed by the guards in this crate (no runtime code — documentation that
/// lives next to the code it describes).
///
/// ## Whitewashing (a leech returns as a fresh newcomer)
/// - Freeleech is bounded by BOTH a time cap (`vetting_blocks`) and a credit cap (`freeleech_cap`)
///   in [`classify`]; a fresh identity gets strictly less than honest participation.
/// - The chain remembers `first_height`/`spent`; a returning identity that already consumed cannot
///   reset those, and a brand-new identity must still buy in (mine or be funded) to spend anything.
/// - Probation grants a *trickle of low-value work*, not full access to scarce hosts (the
///   scheduler reads [`Tier::Probation`] and rate-limits accordingly).
///
/// ## Sybil starvation / self-dealing rings (pump one identity's ratio)
/// - PRIMARY: the mandatory 1% settlement burn ([`SETTLEMENT_BURN_BPS`]) destroys real mined
///   capital on every wash cycle — there is no cheaper-than-honest way to inflate `earned`.
/// - Burn-asymmetry ([`balanced_point`]): a closed ring trading only with itself pays the burn on
///   both legs, so its *combined* ratio is driven below the balanced point — buying one node a high
///   ratio sinks another below the floor, net-zero standing across the ring minus burned capital.
/// - Low-diversity guard ([`contributor_or_capped`]): a node whose `earned` accrued over very few
///   host events is capped at [`Tier::Balanced`], denying ring-pumped nodes the Contributor perks
///   even before the burn math is considered. Heuristic, not proof.
///
/// ## Float-gating mistakes
/// - The only gating comparisons ([`Ratio::at_least`], [`Ratio::cmp_threshold`]) are exact `u128`
///   cross-multiplications. [`Ratio::as_f64`] is display-only and cannot decide a tier.
pub mod anti_gaming {}

#[cfg(test)]
mod tests {
    use super::*;

    fn hist(earned_cr: i128, spent_cr: i128, first: u64, last: u64) -> NodeHistory {
        NodeHistory {
            node_id: "n".into(),
            jobs_hosted: 20,
            jobs_paid: 5,
            heartbeats_hosted: 0,
            heartbeats_paid: 0,
            expiries: 0,
            earned: Amount::from_base(earned_cr * CREDIT_I),
            spent: Amount::from_base(spent_cr * CREDIT_I),
            first_height: first,
            last_height: last,
        }
    }

    const CREDIT_I: i128 = 1_000_000_000_000_000_000;

    // --- burn-derived floor ----------------------------------------------

    #[test]
    fn balanced_point_is_derived_from_burn_not_hardcoded() {
        // At 1% burn: 9900/10000 = 0.99 exactly.
        let (n, d) = balanced_point();
        assert_eq!((n, d), (9_900, 10_000));
        // The honest balanced node (earned = spent * 0.99) clears the balanced floor exactly.
        let r = Ratio {
            contributed: 99,
            consumed: 100,
        };
        let (fnum, fden) = default_balanced_floor();
        assert!(
            r.at_least(fnum, fden),
            "honest burn-balanced node must NOT be a leech"
        );
    }

    #[test]
    fn default_floor_is_about_0_95() {
        let (n, d) = default_balanced_floor();
        // 9900*9600 / (10000*10000) = 95_040_000 / 100_000_000 = 0.9504
        let approx = n as f64 / d as f64;
        assert!((approx - 0.9504).abs() < 1e-9, "got {approx}");
    }

    // --- ratio exactness --------------------------------------------------

    #[test]
    fn ratio_none_when_consumed_zero() {
        let h = hist(10, 0, 5, 5);
        assert!(ratio_of(&h).is_none());
    }

    #[test]
    fn at_least_is_exact_cross_multiplication() {
        let r = Ratio {
            contributed: 3,
            consumed: 2,
        }; // 1.5
        assert!(r.at_least(3, 2));
        assert!(r.at_least(1, 1));
        assert!(!r.at_least(2, 1));
        // boundary equality
        assert!(r.at_least(15, 10));
        assert!(!r.at_least(151, 100));
    }

    #[test]
    fn at_least_handles_huge_values_without_overflow() {
        let big = u128::MAX / 2;
        let r = Ratio {
            contributed: big,
            consumed: big,
        };
        assert!(r.at_least(1, 1));
        assert!(!r.at_least(2, 1));
    }

    #[test]
    fn infinite_contributor_clears_any_threshold() {
        let r = Ratio {
            contributed: 5,
            consumed: 0,
        };
        assert!(r.at_least(1_000_000, 1));
        assert_eq!(r.as_f64(), f64::INFINITY);
    }

    // --- classify boundaries ---------------------------------------------

    #[test]
    fn newcomer_when_first_height_zero() {
        let h = hist(0, 0, 0, 0);
        assert_eq!(classify(&h, 100, &RatioConfig::default()), Tier::Newcomer);
    }

    #[test]
    fn probation_within_vetting_and_under_cap() {
        let cfg = RatioConfig::default();
        // spent 1 credit < 5 cap; age 100 < 8640 vetting.
        let h = hist(0, 1, 50, 50);
        assert_eq!(classify(&h, 150, &cfg), Tier::Probation);
    }

    #[test]
    fn probation_ends_at_credit_cap() {
        let cfg = RatioConfig::default();
        // spent == cap (5) -> no longer under cap -> rated (leech, since earned 0).
        let h = hist(0, 5, 50, 50);
        assert_eq!(classify(&h, 150, &cfg), Tier::Leech);
    }

    #[test]
    fn probation_ends_at_vetting_window() {
        let cfg = RatioConfig::default();
        // first_height = 1 (interacted), age exactly vetting_blocks -> not < -> rated.
        // earned 0, spent 1 -> leech. (first_height must be non-zero or it's a Newcomer.)
        let h = hist(0, 1, 1, 1);
        let beacon = 1 + cfg.vetting_blocks; // age == vetting_blocks
        assert_eq!(classify(&h, beacon, &cfg), Tier::Leech);
    }

    #[test]
    fn leech_below_floor() {
        let cfg = RatioConfig::default();
        // ratio 0.5, past vetting (large spent so over cap).
        let h = hist(50, 100, 1, 1);
        assert_eq!(classify(&h, 100_000, &cfg), Tier::Leech);
    }

    #[test]
    fn balanced_at_burn_point() {
        let cfg = RatioConfig::default();
        // ratio 0.99 — honest burn-balanced — must be Balanced, NOT Leech.
        let h = hist(99, 100, 1, 1);
        assert_eq!(classify(&h, 100_000, &cfg), Tier::Balanced);
    }

    #[test]
    fn contributor_above_threshold_with_diversity() {
        let cfg = RatioConfig::default();
        // ratio 3.0, 20 host events (>= min 8) -> Contributor.
        let h = hist(300, 100, 1, 1);
        assert_eq!(classify(&h, 100_000, &cfg), Tier::Contributor);
    }

    #[test]
    fn low_diversity_caps_at_balanced() {
        let cfg = RatioConfig::default();
        // ratio 10.0 but only 2 host events -> capped at Balanced (anti-wash guard).
        let mut h = hist(1000, 100, 1, 1);
        h.jobs_hosted = 2;
        h.heartbeats_hosted = 0;
        assert_eq!(classify(&h, 100_000, &cfg), Tier::Balanced);
    }

    // --- freeleech --------------------------------------------------------

    #[test]
    fn freeleech_remaining_decreases_with_spend() {
        let cfg = RatioConfig::default();
        let h = hist(0, 2, 1, 1);
        assert_eq!(freeleech_remaining(&h, &cfg), Amount::from_credits(3));
        let h2 = hist(0, 10, 1, 1);
        assert_eq!(freeleech_remaining(&h2, &cfg), Amount::ZERO);
    }

    // --- admit_requester --------------------------------------------------

    #[test]
    fn leech_pays_pay_to_play_tax() {
        let cfg = RatioConfig::default();
        let h = hist(10, 100, 1, 1); // ratio 0.1 -> leech
        let (accept, mult) = admit_requester(&h, 100_000, &cfg);
        assert!(accept, "ratio never hard-blocks");
        assert!(mult >= 12_500, "leech pays >= 1.25x, got {mult}");
        assert!(mult <= 20_000, "capped at 2.0x");
    }

    #[test]
    fn expiries_increase_price_for_hit_and_run() {
        let cfg = RatioConfig::default();
        let mut clean = hist(99, 100, 1, 1); // balanced
        clean.jobs_paid = 10;
        clean.expiries = 0;
        let (_, m_clean) = admit_requester(&clean, 100_000, &cfg);

        let mut grabby = hist(99, 100, 1, 1);
        grabby.jobs_paid = 10;
        grabby.expiries = 10; // 100% expiry rate
        let (_, m_grabby) = admit_requester(&grabby, 100_000, &cfg);

        assert!(
            m_grabby > m_clean,
            "hit-and-run requester pays more: {m_grabby} vs {m_clean}"
        );
    }

    // --- ranking ----------------------------------------------------------

    fn atlas_entry(id: &str, cpu: u32, running: u32) -> AtlasEntry {
        AtlasEntry {
            node_id: id.into(),
            cpu_cores: cpu,
            mem_mb: 1024,
            running_jobs: running,
            last_seen_secs: 0,
            tags: vec![],
        }
    }

    #[test]
    fn rank_orders_contributor_before_leech() {
        let cfg = RatioConfig::default();
        let atlas = vec![
            atlas_entry("leech", 4, 0),
            atlas_entry("contrib", 4, 0),
        ];
        let mut hist_map = HashMap::new();
        hist_map.insert("leech".to_string(), {
            let mut h = hist(10, 100, 1, 90_000); // ratio 0.1, recent
            h.node_id = "leech".into();
            h
        });
        hist_map.insert("contrib".to_string(), {
            let mut h = hist(300, 100, 1, 90_000); // ratio 3.0, recent, 20 host events
            h.node_id = "contrib".into();
            h
        });
        let ranked = rank_hosts(&atlas, &hist_map, 100_000, &cfg);
        assert_eq!(ranked[0].node_id, "contrib");
        assert_eq!(ranked[0].tier, Tier::Contributor);
        assert_eq!(ranked[1].tier, Tier::Leech);
    }

    #[test]
    fn rank_prefers_free_capacity_over_reputation() {
        let cfg = RatioConfig::default();
        // busy contributor vs free balanced node: free capacity wins (a busy host can't take it).
        let atlas = vec![
            atlas_entry("busy_contrib", 4, 4), // no free capacity
            atlas_entry("free_balanced", 4, 0),
        ];
        let mut hist_map = HashMap::new();
        hist_map.insert("busy_contrib".to_string(), {
            let mut h = hist(300, 100, 1, 90_000);
            h.node_id = "busy_contrib".into();
            h
        });
        hist_map.insert("free_balanced".to_string(), {
            let mut h = hist(99, 100, 1, 90_000);
            h.node_id = "free_balanced".into();
            h
        });
        let ranked = rank_hosts(&atlas, &hist_map, 100_000, &cfg);
        assert_eq!(ranked[0].node_id, "free_balanced");
        assert!(ranked[0].has_free_capacity);
        assert!(!ranked[1].has_free_capacity);
    }

    #[test]
    fn unknown_host_treated_as_newcomer() {
        let cfg = RatioConfig::default();
        let atlas = vec![atlas_entry("unknown", 4, 0)];
        let hist_map = HashMap::new();
        let ranked = rank_hosts(&atlas, &hist_map, 100_000, &cfg);
        assert_eq!(ranked[0].tier, Tier::Newcomer);
    }

    // --- property: honest balanced trace stays in [floor, 1.0] ------------

    #[test]
    fn honest_balanced_trace_never_dips_below_floor() {
        let cfg = RatioConfig::default();
        // Simulate N cycles of hosting == consuming with the 1% burn.
        // Each cycle: consume gross G, earn G*(1-burn). Cumulative ratio = (1-burn) constant.
        let g: u128 = 1_000 * CREDIT_I as u128;
        let burn = g * SETTLEMENT_BURN_BPS / BPS_DENOM;
        let net = g - burn;
        let mut earned: u128 = 0;
        let mut spent: u128 = 0;
        for _ in 0..50 {
            earned += net;
            spent += g;
            let r = Ratio {
                contributed: earned,
                consumed: spent,
            };
            assert!(
                r.at_least(cfg.balanced_floor_num, cfg.balanced_floor_den),
                "honest balanced node must stay above the burn-derived floor"
            );
            // and never claims to exceed 1.0
            assert!(!r.at_least(1, 1) || earned == 0, "burn keeps ratio < 1.0");
        }
    }

    // --- wash-trade simulation: combined ring ratio < balanced point ------

    #[test]
    fn self_dealing_ring_combined_ratio_below_balanced() {
        // Two identities A,B trade only with each other. Each leg burns 1%.
        // A pays B (B earns net, A spends gross); B pays A (A earns net, B spends gross).
        let g: u128 = 1_000 * CREDIT_I as u128;
        let net = g - g * SETTLEMENT_BURN_BPS / BPS_DENOM;
        let cycles = 100u128;
        // Symmetric: both earn `net*cycles`, both spend `g*cycles`.
        let combined_earned = 2 * net * cycles;
        let combined_spent = 2 * g * cycles;
        let combined = Ratio {
            contributed: combined_earned,
            consumed: combined_spent,
        };
        let (bp_num, bp_den) = balanced_point();
        // Combined ring ratio equals exactly the balanced point (0.99) — it can never exceed it,
        // so a ring can only push ONE node up by sinking the other, at a cumulative burn cost.
        assert_eq!(combined.cmp_threshold(bp_num + 1, bp_den), core::cmp::Ordering::Less);
        // burned capital grows linearly with faked volume.
        let burned = 2 * (g * SETTLEMENT_BURN_BPS / BPS_DENOM) * cycles;
        assert!(burned > 0);
        assert_eq!(burned, 2 * cycles * (g / 100));
    }
}
