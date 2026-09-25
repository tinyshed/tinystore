package records

// a bloom filter spends 10 bits on each distinct value it holds, and 7 hashes
// leave about one false positive in a hundred
const (
	bloomBits   = 10
	bloomHashes = 7
)

func bloomOf(values []string) []byte {
	distinct := map[string]bool{}
	for _, value := range values {
		distinct[value] = true
	}
	filter := make([]byte, max(8, (len(distinct)*bloomBits+7)/8))
	size := uint64(len(filter)) * 8
	for value := range distinct {
		a, b := bloomHash(value)
		for i := range uint64(bloomHashes) {
			bit := (a + i*b) % size
			filter[bit/8] |= 1 << (bit % 8)
		}
	}
	return filter
}

func bloomMayHold(filter []byte, value string) bool {
	if len(filter) == 0 {
		return false
	}
	size := uint64(len(filter)) * 8
	a, b := bloomHash(value)
	for i := range uint64(bloomHashes) {
		if bit := (a + i*b) % size; filter[bit/8]&(1<<(bit%8)) == 0 {
			return false
		}
	}
	return true
}

// bloomHash is FNV-1a and a mix of it, the two hashes double hashing needs
func bloomHash(value string) (a, b uint64) {
	hash := uint64(14695981039346656037)
	for i := range len(value) {
		hash = (hash ^ uint64(value[i])) * 1099511628211
	}
	mixed := (hash ^ hash>>31) * 0x9e3779b97f4a7c15
	return hash, mixed | 1
}

// idLike picks the attribute columns a lookup names one value of: short JSON
// strings nearly all distinct, as request ids, hashes and uuids are. Distinct
// numbers are times and measures, which a range asks for instead.
func idLike(values []string) bool {
	if len(values) < 16 {
		return false
	}
	distinct := map[string]bool{}
	for _, value := range values {
		if _, quoted := unquote(value); !quoted || len(value) > 64 {
			return false
		}
		distinct[value] = true
	}
	return len(distinct)*10 >= len(values)*9
}
