module github.com/ce-net/ce-ratio/clients/go

go 1.21

// The trust ceapp's Go SDK rides the core substrate client's transport hatch (ce-go) and reports
// the money unit (History.Earned/Spent) with the economy ceapp's Amount (economy-adapter Go SDK).
// Cross-repo CE deps are github git deps; the replaces resolve them to local working trees for dev.
require (
	github.com/ce-net/ce-go v0.0.0
	github.com/ce-net/economy-adapter/clients/go v0.0.0
)

replace github.com/ce-net/ce-go => ../../../ce-go

replace github.com/ce-net/economy-adapter/clients/go => ../../../economy-adapter/clients/go
