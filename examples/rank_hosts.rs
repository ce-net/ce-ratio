//! Rank the atlas hosts by share-ratio reputation, best-first.
//!
//! I am a requester picking a host. This example pulls `/atlas` and `/beacon` from the local node
//! via `ce-rs`, fetches each candidate's `/history`, then orders them with
//! `ce_ratio::rank_hosts`. The output is a *preference order* — it never authorizes placement; a
//! valid `ce-cap` capability chain is still required to actually run work on any host.
//!
//! Run against a live local node:
//!
//! ```sh
//! cargo run --example rank_hosts
//! ```

use std::collections::HashMap;

use ce_ratio::{rank_hosts, RatioConfig};
use ce_rs::CeClient;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let client = CeClient::local();
    let cfg = RatioConfig::default();

    let atlas = client.atlas().await?;
    let beacon = client.beacon().await?;

    if atlas.is_empty() {
        println!("atlas is empty — no peers advertising capacity yet");
        return Ok(());
    }

    // Fetch history for each candidate. A short-TTL cache keyed by (node_id, last_height) would
    // amortize this over a large atlas (see PLAN risk note); kept simple here.
    let mut hist = HashMap::new();
    for entry in &atlas {
        match client.history(&entry.node_id).await {
            Ok(h) => {
                hist.insert(entry.node_id.clone(), h);
            }
            Err(e) => {
                // Treat an unfetchable history as a stranger (rank_hosts does this for missing keys).
                eprintln!("history fetch failed for {}: {e}", short(&entry.node_id));
            }
        }
    }

    let ranked = rank_hosts(&atlas, &hist, beacon.height, &cfg);

    // Plain, aligned, no-emoji output per CE terminal design rules.
    println!("rank  node       tier         recency  capacity");
    for (i, r) in ranked.iter().enumerate() {
        let recency = format!("{:.2}x", r.recency_bps as f64 / 10_000.0);
        let cap = if r.has_free_capacity { "free" } else { "busy" };
        println!(
            "{:>4}  {:<9}  {:<11}  {:>7}  {}",
            i + 1,
            short(&r.node_id),
            tier_label(r.tier),
            recency,
            cap
        );
    }

    Ok(())
}

fn short(node_id: &str) -> String {
    if node_id.len() > 8 {
        format!("{}..{}", &node_id[..4], &node_id[node_id.len() - 2..])
    } else {
        node_id.to_string()
    }
}

fn tier_label(t: ce_ratio::Tier) -> &'static str {
    match t {
        ce_ratio::Tier::Newcomer => "Newcomer",
        ce_ratio::Tier::Probation => "Probation",
        ce_ratio::Tier::Leech => "Leech",
        ce_ratio::Tier::Balanced => "Balanced",
        ce_ratio::Tier::Contributor => "Contributor",
    }
}
