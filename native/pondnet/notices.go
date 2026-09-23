package pondnet

import _ "embed"

// ThirdPartyNotices accompanies the network helper's redistributed dependencies.
// Regenerate with scripts/update-network-notices.py when go.mod/go.sum change.
//
//go:embed THIRD_PARTY_NOTICES.txt
var ThirdPartyNotices string
