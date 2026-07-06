package ratio

import (
	"context"
	"net/http"
	"net/http/httptest"
	"testing"

	ce "github.com/ce-net/ce-go"
)

func TestHistoryDecodesWithEconomyAmounts(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/history/peer9" {
			http.Error(w, "wrong path", 404)
			return
		}
		w.Write([]byte(`{"node_id":"peer9","jobs_hosted":3,"jobs_paid":1,"heartbeats_hosted":10,` +
			`"heartbeats_paid":2,"expiries":0,"earned":"2500000000000000000","spent":"1000000000000000000",` +
			`"first_height":5,"last_height":42}`))
	}))
	defer srv.Close()
	tc := New(ce.Connect(ce.WithBaseURL(srv.URL), ce.WithToken("t")))
	h, err := tc.History(context.Background(), "peer9")
	if err != nil {
		t.Fatal(err)
	}
	if h.NodeID != "peer9" || h.JobsHosted != 3 {
		t.Fatalf("bad history: %+v", h)
	}
	if h.Earned.Credits() != "2.5" || h.Spent.Credits() != "1" {
		t.Fatalf("economy amount decode: earned=%s spent=%s", h.Earned.Credits(), h.Spent.Credits())
	}
	if h.IsNewcomer() {
		t.Fatal("first_height 5 is not a newcomer")
	}
	if h.DeliveredWork() != 13 {
		t.Fatalf("delivered work = %d, want 13", h.DeliveredWork())
	}
}
