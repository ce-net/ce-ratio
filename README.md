# ce-ratio

Torrent-style **compute share-ratio and reputation** for CE, computed in the SDK/app tier over
[`ce-rs`](../ce-rs). It turns the immutable on-chain facts the node already serves (`/history`,
`/atlas`, `/beacon`) into one number — `ratio = contributed / consumed` — and uses it to **rank,
order, and price** scheduling choices. Zero node and zero consensus changes.

> Design: `PLAN/02-ratio-economy.md`.

## HARD RULE: rank/order only — never authorization

**`ce-ratio` output may RANK or ORDER candidates only. It must NEVER be importable by, nor feed,
`ce-cap` capability verification.** Capabilities are CE's *only* authorization primitive. A high
ratio does not grant a capability; a low ratio does not deny one. Ratio only biases which
already-permitted choice a caller prefers, what price a host quotes, and how much verification to
apply.

This is enforced structurally: **ce-ratio depends only on `ce-rs`** (a thin HTTP client) and has
**no dependency on `ce-cap`, `ce-identity`, or any node/consensus crate.** It is impossible to wire
a ratio into a capability decision through this code. If you ever want to `use ce_ratio::...` inside
`ce-cap`, that is the bug this boundary exists to prevent.

## The formula

```
contributed C(n) = NodeHistory.earned   // earned as host, NET of the 1% settlement burn
consumed    S(n) = NodeHistory.spent    // spent as cell, GROSS (pre-burn)
ratio(n)         = C(n) / S(n)           // exact (u128, u128) rational — never f64 on the gating path
```

Money is integer base units (`i128`, `10^18` per credit). The gating comparison `Ratio::at_least`
is a float-free `u128` cross-multiplication; `Ratio::as_f64` exists for **display only**.

### Burn-asymmetry floor — DERIVED, not hardcoded

Because `earned` is post-burn and `spent` is gross, an honest node that hosts exactly what it
consumes lands at `ratio = (BPS_DENOM - SETTLEMENT_BURN_BPS) / BPS_DENOM = 0.99` at the shipped 1%
burn — not `1.0`. The "balanced floor" is **derived** from `SETTLEMENT_BURN_BPS` (see
`balanced_point()` / `default_balanced_floor()`), so if the chain's burn rate ever changes the
floor moves with it. There is no magic `0.95` in the code.

## Tiers

| Tier | ratio vs floor | as requester picking hosts | as host admitting a bid |
|---|---|---|---|
| `Newcomer` | n/a | trickle of low-value work | accept up to freeleech cap, elevated verification |
| `Probation` | n/a (bounded freeleech) | same, until vetting/cap exhausted | accept, elevated verification |
| `Leech` | `< floor` | de-prioritized, not blocked | accept at 1.25–2.0x price (pay-to-play) |
| `Balanced` | `floor – 2.0` | normal priority | accept at 1.0x |
| `Contributor` | `>= 2.0` | priority access to scarce hosts | accept at 1.0x |

The only hard gate is the capability chain. Tiers re-price and re-order; they never block.

## Surface

```rust
ratio_of(&NodeHistory) -> Option<Ratio>            // None when consumed == 0 (infinite contributor)
classify(&NodeHistory, beacon_height, &RatioConfig) -> Tier
rank_hosts(&[AtlasEntry], &HashMap<String, NodeHistory>, beacon_height, &RatioConfig) -> Vec<RankedHost>
rank_host_indices(...) -> Vec<usize>
admit_requester(&NodeHistory, beacon_height, &RatioConfig) -> (accept: bool, price_bps: u32)
freeleech_remaining(&NodeHistory, &RatioConfig) -> Amount
fetch_and_classify(&CeClient, node_id, &RatioConfig) -> Result<(Tier, Option<Ratio>)>  // async convenience
```

`rank_hosts` orders best-first by: free capacity → tier → recency → lower load → node_id (stable).

## Anti-gaming

- **Whitewashing** (abandon a leech identity, return as a fresh newcomer): freeleech is bounded by
  *both* a time cap (`vetting_blocks`) and a credit cap (`freeleech_cap`); the chain remembers
  `first_height`/`spent`; Probation is a supervised trickle, not full access.
- **Sybil starvation / self-dealing rings**: the mandatory settlement burn makes inflating `earned`
  capital-costly; burn-asymmetry drives any closed ring's *combined* ratio below the balanced point;
  a low-diversity guard caps nodes whose `earned` accrued over very few host events at `Balanced`.

See the `anti_gaming` module docs in `src/lib.rs` for the per-guard detail, and the wash-trade
simulation in the test suite.

## Time decay (v1 proxy)

`/history` is cumulative-only, so true windowed decay is impossible in the SDK. v1 ships a
**height-rate recency proxy** (`src/decay.rs`): a node active near the tip gets full weight, a
dormant one decays toward a floor. The recency factor only de-prioritizes for *display/ordering* —
it never touches the exact rational used for hard threshold comparisons. (v2 windowed history is a
deferred, observational node change; see the design doc.)

## Example

```sh
cargo run --example rank_hosts   # ranks the local node's atlas, best-first
```

## Test

```sh
cargo test
```
