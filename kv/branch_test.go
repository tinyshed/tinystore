package kv

import "testing"

// a branch packs the length of each prefix above its rows in the bits of its
// depth, and says when it lies deeper than they reach
func TestABranchPacksItsPrefixLengthsByDepth(t *testing.T) {
	state := openTestState(t, t.TempDir())
	drafts := openTestBucket[string](t, state, "drafts")
	if drafts.hidden != 0 {
		t.Fatalf("the root packs %d", drafts.hidden)
	}
	if packed := drafts.Of("tenant-7", 42).hidden; packed != 10|14<<10 {
		t.Fatalf("Of(tenant-7, 42) packs %d, not 14346", packed)
	}
	if deep := drafts.Of(1, 2, 3, 4, 5, 6, 7).hidden; deep>>deeperBit&1 != 1 {
		t.Fatalf("seven owners pack %d, not the bit past the lengths", deep)
	}
}
