//! Reputation history — the TRUST ceapp owns this, not the substrate and not the economy ceapp.
//!
//! Per the modularity law, a node's interaction history (the reputation substrate) is a TRUST
//! concept. `ce-ratio` is the trust ceapp; it owns [`NodeHistory`] and the `history()` read. The
//! money scalar it reports (`earned`/`spent`) comes from the economy ceapp's SDK (`ce_economy::
//! Amount`) — trust depends on economy for the unit, both sit on the chain ceapp. The core SDK
//! (`ce-rs`) carries NONE of this; we read `/history/:node_id` through its substrate transport
//! hatch. RANK/ORDER ONLY — never authorization (see the crate-level HARD RULE).

use anyhow::Result;
use ce_economy::Amount;
use ce_rs::CeClient;
use serde::Deserialize;

/// A node's interaction history — the reputation substrate. Immutable facts from the chain ceapp;
/// derive your own trust from them. `earned`/`spent` are `ce_economy::Amount` (the money unit).
#[derive(Debug, Clone, Deserialize)]
pub struct NodeHistory {
    pub node_id: String,
    pub jobs_hosted: u64,
    pub jobs_paid: u64,
    pub heartbeats_hosted: u64,
    pub heartbeats_paid: u64,
    pub expiries: u64,
    pub earned: Amount,
    pub spent: Amount,
    pub first_height: u64,
    pub last_height: u64,
}

impl NodeHistory {
    /// A node with no recorded interactions — a stranger (bottom of the trust gradient).
    pub fn is_newcomer(&self) -> bool {
        self.first_height == 0
    }

    /// A simple default trust heuristic: total work this host delivered and was paid for
    /// (settled jobs + heartbeats received). Higher = more proven. Apps may define their own.
    pub fn delivered_work(&self) -> u64 {
        self.jobs_hosted + self.heartbeats_hosted
    }
}

/// Read a node's interaction history — the reputation substrate (`GET /history/:node_id`).
/// Reads over the core SDK's substrate transport hatch; the TYPE and meaning are the trust
/// ceapp's, not the core SDK's.
pub async fn history(client: &CeClient, node_id: &str) -> Result<NodeHistory> {
    client.get_json(&format!("/history/{node_id}")).await
}
