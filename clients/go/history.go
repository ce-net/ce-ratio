// Package ratio is the trust ceapp's typed Go SDK — reputation history over CE.
//
// Per the CE modularity law, a node's interaction history (the reputation substrate) is a TRUST
// concept, not a substrate one and not the economy's. ce-ratio is the trust ceapp; this package
// owns the History type and its read. It rides the core substrate client's transport hatch
// (github.com/ce-net/ce-go, Client.Do) and reports the money unit (Earned/Spent) using the economy
// ceapp's Amount (github.com/ce-net/economy-adapter/clients/go) — trust depends on economy for the
// unit. RANK/ORDER ONLY — this is never authorization input. Mirrors Rust ce_ratio::NodeHistory.
package ratio

import (
	"context"
	"encoding/json"
	"net/http"

	ce "github.com/ce-net/ce-go"
	economy "github.com/ce-net/economy-adapter/clients/go"
)

// History is a node's cumulative interaction record (GET /history/:node_id) — the reputation
// substrate. Immutable facts; derive your own trust from them. Earned/Spent are economy.Amount.
type History struct {
	NodeID           string         `json:"node_id"`
	JobsHosted       uint64         `json:"jobs_hosted"`
	JobsPaid         uint64         `json:"jobs_paid"`
	HeartbeatsHosted uint64         `json:"heartbeats_hosted"`
	HeartbeatsPaid   uint64         `json:"heartbeats_paid"`
	Expiries         uint64         `json:"expiries"`
	Earned           economy.Amount `json:"earned"`
	Spent            economy.Amount `json:"spent"`
	FirstHeight      uint64         `json:"first_height"`
	LastHeight       uint64         `json:"last_height"`
}

// IsNewcomer reports whether the node has no recorded interactions — a stranger (bottom of the
// trust gradient).
func (h History) IsNewcomer() bool { return h.FirstHeight == 0 }

// DeliveredWork is a simple default trust heuristic: total work this host delivered (hosted jobs +
// heartbeats). Higher = more proven. Apps may define their own.
func (h History) DeliveredWork() uint64 { return h.JobsHosted + h.HeartbeatsHosted }

// TrustClient reads reputation over a core substrate client's transport hatch.
type TrustClient struct {
	ce *ce.Client
}

// New wraps an existing core substrate client.
func New(c *ce.Client) *TrustClient { return &TrustClient{ce: c} }

// Connect opens a trust client over the local node (the trust-tier counterpart of ce.Connect).
func Connect(opts ...ce.Option) *TrustClient { return &TrustClient{ce: ce.Connect(opts...)} }

// Client returns the underlying substrate client.
func (t *TrustClient) Client() *ce.Client { return t.ce }

// History returns a node's interaction history (GET /history/:node_id). The type and meaning are
// the trust ceapp's; the read rides the core SDK's substrate transport hatch.
func (t *TrustClient) History(ctx context.Context, nodeID string) (History, error) {
	raw, err := t.ce.Do(ctx, http.MethodGet, "/history/"+nodeID, nil)
	if err != nil {
		return History{}, err
	}
	var h History
	if err := json.Unmarshal(raw, &h); err != nil {
		return History{}, err
	}
	return h, nil
}
