package records

import "sync"

// followCacheBytes is how many bytes of blocks and segment rows Follow keeps
// for the batches after it; every Follow reserves them beside its own weight
const followCacheBytes = 4 << 20

// followCache keeps the block bodies and segment rows Follow fetched, so that a
// follower far behind fetches a merged segment's block once rather than once
// for every place it holds.
//
// The places of a quiet stream lie hours apart in the order segments were
// sealed, and each batch that reaches one needs the same block again:
//
//	places 1 4 7 9, held by 1 in one block; batches reach them one at a time
//	without the cache   the block is fetched four times
//	with it             once
//
// An entry never goes stale: block ids never repeat, and a holder's row changes
// only with its first block.
type followCache struct {
	mu sync.Mutex
	// recent fills first. Once it holds half the cache's bytes it becomes
	// older, and older's entries move back to recent when they are used.
	recent, older map[cacheKey][]byte
	recentBytes   int
}

// cacheKey names a block by its id, and a segment row by its id and its first block's
type cacheKey struct {
	segment, block int64
}

func blockCacheKey(id int64) cacheKey {
	return cacheKey{block: id}
}

func rowCacheKey(held *heldSegment) cacheKey {
	return cacheKey{segment: held.id, block: held.firstBlock}
}

func (c *followCache) get(key cacheKey) ([]byte, bool) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if body, ok := c.recent[key]; ok {
		return body, true
	}
	body, ok := c.older[key]
	if ok {
		c.keep(key, body)
	}
	return body, ok
}

func (c *followCache) put(key cacheKey, body []byte) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.keep(key, body)
}

// keep adds an entry to the recent half, which becomes the older half once it
// holds half the cache's bytes; an entry larger than half is not kept
func (c *followCache) keep(key cacheKey, body []byte) {
	half := followCacheBytes / 2
	if len(body) > half {
		return
	}
	if c.recent == nil || c.recentBytes+len(body) > half {
		c.older = c.recent
		c.recent = map[cacheKey][]byte{}
		c.recentBytes = 0
	}
	if _, ok := c.recent[key]; !ok {
		c.recent[key] = body
		c.recentBytes += len(body)
	}
}
