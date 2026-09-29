package records

import (
	"testing"
	"time"
)

// A follower far behind fetches each block of a merged segment once, however
// many of its places and batches reach it. This is the example in the comment
// on followCache.
func TestAFollowerFetchesAMergedBlockOnce(t *testing.T) {
	s := openRecords(t)
	for range 4 {
		s.hourly(t, "a", 30)
		s.hourly(t, "b", 25)
		s.clock.advance(time.Hour)
		s.maintain(t)
	}
	if c := s.counts(t); c.blocks != 2 {
		t.Fatalf("four hours of two streams left %d blocks, want one each", c.blocks)
	}
	if followed, _, _ := s.followAll(t, Cursor{}, 7); len(followed) != 220 {
		t.Fatalf("followed %d records, want 220", len(followed))
	}
	if fetched := s.Stats().ReadBlocks; fetched != 2 {
		t.Fatalf("fetched %d blocks, want each of the two once", fetched)
	}
}

// the cache holds at most its bytes, drops what is not used again, and keeps
// what is
func TestTheFollowCacheKeepsWhatIsUsedWithinItsBytes(t *testing.T) {
	var cache followCache
	body := make([]byte, followCacheBytes/8)
	cache.put(blockCacheKey(1), body)
	for id := int64(2); id <= 20; id++ {
		if _, ok := cache.get(blockCacheKey(1)); !ok {
			t.Fatalf("block 1, used before every put, was dropped before block %d", id)
		}
		cache.put(blockCacheKey(id), body)
		if held := cache.heldBytes(); held > followCacheBytes {
			t.Fatalf("after block %d the cache holds %d bytes", id, held)
		}
	}
	if _, ok := cache.get(blockCacheKey(2)); ok {
		t.Fatal("block 2, never used again, is still held")
	}
	cache.put(blockCacheKey(21), make([]byte, followCacheBytes))
	if _, ok := cache.get(blockCacheKey(21)); ok {
		t.Fatal("a block larger than half the cache is held")
	}
}

// heldBytes is what the cache's entries hold, one moved back to the recent half counted once
func (c *followCache) heldBytes() int {
	held := c.recentBytes
	for key, body := range c.older {
		if _, moved := c.recent[key]; !moved {
			held += len(body)
		}
	}
	return held
}
