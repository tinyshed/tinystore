// Package spike measures the built server through `tinystore serve` against
// the round that designed it, docs/reports/rpc-mechanics-2026-09-28.md: the
// same bucket, the same calls and the same clients' languages, run beside the
// prototype `spike/rpc_*` of the root module. The measurements are skipped
// unless TINYSTORE_SPIKE is set:
//
//	TINYSTORE_SPIKE=1 go -C server test ./spike -run '^TestServe' -v -count=1
package spike
