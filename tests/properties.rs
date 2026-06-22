//! Property and fuzz tests for ce-ratio.
//!
//! These complement the in-crate unit tests with randomized coverage of the load-bearing
//! invariants the design depends on:
//!   * the float-free gating comparison ([`Ratio::at_least`] / [`Ratio::cmp_threshold`]) is EXACT
//!     and never overflows, even at `u128::MAX` operands (the 256-bit `wide_mul` path);
//!   * `at_least` and `cmp_threshold` agree;
//!   * `as_f64` is display-only and never flips a gating decision near the threshold;
//!   * classification is monotone in the ratio and respects the burn-derived floor;
//!   * recency decay is monotone non-increasing and bounded `[floor, FULL]`;
//!   * `admit_requester` always stays within the design's `[1.0x, 2.0x]` band and never blocks;
//!   * ranking is a total, deterministic order that prefers free capacity then tier;
//!   * the HARD rule — ce-ratio depends on NO node/consensus/cap crate — is asserted structurally.

use std::collections::HashMap;

use ce_ratio::{
    Ratio, RatioConfig, Tier, admit_requester, balanced_point, classify, default_balanced_floor,
    rank_hosts, ratio_of, recency_factor_bps,
};
use ce_rs::{Amount, AtlasEntry, NodeHistory};
use proptest::prelude::*;

const CREDIT_I: i128 = 1_000_000_000_000_000_000;

fn hist(earned_base: i128, spent_base: i128, first: u64, last: u64) -> NodeHistory {
    NodeHistory {
        node_id: "n".into(),
        jobs_hosted: 50,
        jobs_paid: 5,
        heartbeats_hosted: 0,
        heartbeats_paid: 0,
        expiries: 0,
        earned: Amount::from_base(earned_base),
        spent: Amount::from_base(spent_base),
        first_height: first,
        last_height: last,
    }
}

/// Reference cross-multiplication using `u128::checked_mul`; `None` when the exact product would
/// overflow `u128` (in which case the ce-ratio wide path is the only authority and we skip).
fn ref_at_least(contributed: u128, consumed: u128, num: u128, den: u128) -> Option<bool> {
    if num == 0 {
        return Some(true);
    }
    if consumed == 0 {
        return Some(true);
    }
    let lhs = contributed.checked_mul(den)?;
    let rhs = consumed.checked_mul(num)?;
    Some(lhs >= rhs)
}

proptest! {
    // The exact gating comparison agrees with a u128-checked reference whenever the reference can
    // be computed without overflow. This pins down `wide_mul`/`cmp_u128_products` correctness.
    #[test]
    fn at_least_matches_reference(
        contributed in 0u128..u128::MAX,
        consumed in 0u128..u128::MAX,
        num in 0u128..1_000_000u128,
        den in 1u128..1_000_000u128,
    ) {
        let r = Ratio { contributed, consumed };
        if let Some(expected) = ref_at_least(contributed, consumed, num, den) {
            prop_assert_eq!(r.at_least(num, den), expected,
                "ratio {}/{} vs {}/{}", contributed, consumed, num, den);
        }
    }

    // No input — including u128::MAX on every operand — may panic or overflow. The 256-bit
    // multiply must absorb it. We only assert it returns (does not panic) and is self-consistent.
    #[test]
    fn at_least_never_overflows_at_extremes(
        contributed in (u128::MAX - 16)..=u128::MAX,
        consumed in (u128::MAX - 16)..=u128::MAX,
        num in (u128::MAX - 16)..=u128::MAX,
        den in (u128::MAX - 16)..=u128::MAX,
    ) {
        let r = Ratio { contributed, consumed };
        let a = r.at_least(num, den);
        // cmp_threshold must agree with at_least at the same threshold.
        let c = r.cmp_threshold(num, den);
        let from_cmp = matches!(c, std::cmp::Ordering::Greater | std::cmp::Ordering::Equal);
        prop_assert_eq!(a, from_cmp);
    }

    // `at_least(num,den)` is equivalent to `cmp_threshold(num,den) >= Equal` for ALL inputs.
    #[test]
    fn at_least_and_cmp_threshold_agree(
        contributed in 0u128..u128::MAX,
        consumed in 0u128..u128::MAX,
        num in 0u128..u128::MAX,
        den in 1u128..u128::MAX,
    ) {
        let r = Ratio { contributed, consumed };
        let from_cmp = matches!(
            r.cmp_threshold(num, den),
            std::cmp::Ordering::Greater | std::cmp::Ordering::Equal
        );
        prop_assert_eq!(r.at_least(num, den), from_cmp);
    }

    // Monotonicity in the numerator threshold: if a node clears num/den, it also clears any SMALLER
    // numerator with the same denominator (a stricter target is harder).
    #[test]
    fn at_least_monotone_in_threshold(
        contributed in 0u128..(1u128 << 80),
        consumed in 1u128..(1u128 << 80),
        num in 0u128..10_000u128,
        den in 1u128..10_000u128,
    ) {
        let r = Ratio { contributed, consumed };
        if r.at_least(num, den) && num > 0 {
            prop_assert!(r.at_least(num - 1, den), "clearing num must clear num-1");
        }
    }

    // classify is monotone in earned: raising `earned` (holding spent/age fixed) never lowers the
    // tier rank. A node that contributes more is never rated worse.
    #[test]
    fn classify_monotone_in_earned(
        earned_a in 0i128..1_000_000i128,
        delta in 0i128..1_000_000i128,
        spent in 1i128..1_000_000i128,
    ) {
        let cfg = RatioConfig::default();
        // Past vetting so the ratio (not probation) governs: large age + spent over cap.
        let beacon = 1_000_000u64;
        let lo = hist(earned_a * CREDIT_I, (spent + 10) * CREDIT_I, 1, 1);
        let hi = hist((earned_a + delta) * CREDIT_I, (spent + 10) * CREDIT_I, 1, 1);
        let t_lo = classify(&lo, beacon, &cfg).rank_score();
        let t_hi = classify(&hi, beacon, &cfg).rank_score();
        prop_assert!(t_hi >= t_lo, "more earned must not lower tier: {} -> {}", t_lo, t_hi);
    }

    // recency is bounded [floor, FULL] and monotone non-increasing as dormancy grows.
    #[test]
    fn recency_bounded_and_monotone(
        last in 1u64..100_000u64,
        active in 0u64..10_000u64,
        decay in 1u64..100_000u64,
        floor in 0u32..10_000u32,
        step in 1u64..5_000u64,
    ) {
        let tip0 = last + 1;
        let tip1 = tip0 + step;
        let f0 = recency_factor_bps(last, tip0, active, decay, floor);
        let f1 = recency_factor_bps(last, tip1, active, decay, floor);
        prop_assert!(f0 <= 10_000 && f0 >= floor.min(10_000));
        prop_assert!(f1 <= 10_000 && f1 >= floor.min(10_000));
        prop_assert!(f1 <= f0, "more dormant must not raise recency: {} -> {}", f0, f1);
    }

    // admit_requester NEVER blocks (ratio is soft) and the multiplier always lands in [1.0x, 2.0x].
    #[test]
    fn admit_always_in_band(
        earned in 0i128..1_000_000i128,
        spent in 0i128..1_000_000i128,
        jobs_paid in 0u64..1000,
        expiries in 0u64..1000,
        first in 0u64..200_000,
    ) {
        let cfg = RatioConfig::default();
        let mut h = hist(earned * CREDIT_I, spent * CREDIT_I, first, first);
        h.jobs_paid = jobs_paid;
        h.expiries = expiries;
        let (accept, mult) = admit_requester(&h, 1_000_000, &cfg);
        prop_assert!(accept, "ratio must never hard-block");
        prop_assert!((10_000..=20_000).contains(&mult), "mult {} out of [1.0x,2.0x]", mult);
    }
}

// --- Deterministic edge cases the proptests don't pin precisely -----------------------------

#[test]
fn ratio_of_clamps_negative_defensively() {
    // Cumulative totals should never be negative, but a malformed feed must not panic — it clamps.
    let h = hist(-5 * CREDIT_I, -3 * CREDIT_I, 1, 1);
    // consumed clamps to 0 -> None (infinite contributor), never a panic or negative u128.
    assert!(ratio_of(&h).is_none());
}

#[test]
fn ratio_of_negative_earned_clamps_to_zero_contributed() {
    let h = hist(-5 * CREDIT_I, 10 * CREDIT_I, 1, 1);
    let r = ratio_of(&h).expect("has consumption");
    assert_eq!(r.contributed, 0, "negative earned clamps to 0");
    assert_eq!(r.consumed, 10 * CREDIT_I as u128);
}

#[test]
fn as_f64_is_display_only_infinity_for_zero_consumed() {
    let r = Ratio { contributed: 1, consumed: 0 };
    assert_eq!(r.as_f64(), f64::INFINITY);
    // And gating still says "clears" without ever touching the float.
    assert!(r.at_least(u128::MAX, 1));
}

#[test]
fn balanced_floor_is_strictly_below_balanced_point() {
    // The floor must sit below the exact burn-balanced point so honest nodes are never leeches.
    let (bp_n, bp_d) = balanced_point();
    let (f_n, f_d) = default_balanced_floor();
    // f_n/f_d < bp_n/bp_d  <=>  f_n*bp_d < bp_n*f_d
    assert!(f_n * bp_d < bp_n * f_d, "floor must be below the balanced point");
}

#[test]
fn honest_burn_balanced_node_clears_floor_exactly() {
    // earned = spent * (1 - burn): the canonical honest node. Must clear the floor at any scale.
    let cfg = RatioConfig::default();
    for scale in [1u128, 1000, CREDIT_I as u128, (CREDIT_I as u128) * 1_000_000] {
        let spent = 100 * scale;
        let earned = spent * (10_000 - 100) / 10_000; // 1% burn
        let r = Ratio { contributed: earned, consumed: spent };
        assert!(
            r.at_least(cfg.balanced_floor_num, cfg.balanced_floor_den),
            "honest burn-balanced node must clear the floor at scale {scale}"
        );
    }
}

#[test]
fn rank_is_deterministic_and_stable() {
    // Same input -> identical output order across runs (no nondeterministic sort).
    let cfg = RatioConfig::default();
    let atlas: Vec<AtlasEntry> = (0..8)
        .map(|i| AtlasEntry {
            node_id: format!("node{i:02}"),
            cpu_cores: 4,
            mem_mb: 1024,
            running_jobs: i % 4,
            last_seen_secs: 0,
            tags: vec![],
        })
        .collect();
    let mut hist_map = HashMap::new();
    for i in 0..8 {
        let mut h = hist((i as i128) * 50 * CREDIT_I, 50 * CREDIT_I, 1, 90_000);
        h.node_id = format!("node{i:02}");
        h.jobs_hosted = 20;
        hist_map.insert(format!("node{i:02}"), h);
    }
    let a = rank_hosts(&atlas, &hist_map, 100_000, &cfg);
    let b = rank_hosts(&atlas, &hist_map, 100_000, &cfg);
    let ids_a: Vec<_> = a.iter().map(|r| r.node_id.clone()).collect();
    let ids_b: Vec<_> = b.iter().map(|r| r.node_id.clone()).collect();
    assert_eq!(ids_a, ids_b, "ranking must be deterministic");
    // Free-capacity hosts (running < cpu) must all precede saturated ones.
    let first_busy = a.iter().position(|r| !r.has_free_capacity);
    if let Some(busy_idx) = first_busy {
        assert!(
            a[..busy_idx].iter().all(|r| r.has_free_capacity),
            "all free-capacity hosts must rank before any saturated host"
        );
    }
}

#[test]
fn empty_atlas_ranks_to_empty() {
    let cfg = RatioConfig::default();
    let ranked = rank_hosts(&[], &HashMap::new(), 100, &cfg);
    assert!(ranked.is_empty());
}

#[test]
fn classify_newcomer_independent_of_beacon() {
    // first_height == 0 is always Newcomer regardless of beacon height (even 0).
    let cfg = RatioConfig::default();
    let h = hist(0, 0, 0, 0);
    for beacon in [0u64, 1, 8_640, u64::MAX] {
        assert_eq!(classify(&h, beacon, &cfg), Tier::Newcomer);
    }
}

/// The HARD rule, asserted structurally: ce-ratio's own manifest must NOT depend on ce-cap,
/// ce-identity, or any node/consensus crate. If someone wires a ratio into a capability decision
/// by adding such a dependency, this test fails loudly. (The crate compiling at all already proves
/// no `use ce_cap::...` exists in its source, since that import would not resolve.)
#[test]
fn ratio_manifest_has_no_capability_or_consensus_dep() {
    let manifest = include_str!("../Cargo.toml");
    // Strip out the [dev-dependencies] section so test-only deps don't trip this (there are none
    // of these there anyway, but be explicit about the boundary).
    let deps_section = manifest
        .split("[dev-dependencies]")
        .next()
        .unwrap_or(manifest);
    for forbidden in ["ce-cap", "ce-identity", "ce-chain", "ce-mesh", "ce-node", "ce-protocol"] {
        assert!(
            !deps_section.contains(forbidden),
            "ce-ratio must never depend on {forbidden}: ranking must not feed capability verification"
        );
    }
}

#[test]
fn beacon_below_first_height_does_not_panic() {
    // A stale/forked beacon below first_height must saturate, not underflow.
    let cfg = RatioConfig::default();
    let h = hist(0, 1 * CREDIT_I, 1000, 1000);
    // beacon < first_height: age saturates to 0 -> still within vetting window -> Probation.
    let t = classify(&h, 5, &cfg);
    assert!(matches!(t, Tier::Probation | Tier::Leech | Tier::Newcomer));
}
