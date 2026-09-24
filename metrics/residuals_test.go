package metrics

import (
	"math/rand/v2"
	"testing"
)

func TestResidualModesAreBoundedAndExact(t *testing.T) {
	s := encodingStore(t)
	random := rand.New(rand.NewPCG(7, 8))
	for count := range 240 {
		values := make([]int64, count)
		for i := range values {
			switch count % 4 {
			case 1:
				values[i] = int64(random.IntN(3)) - 1
			case 2:
				if i%27 == 0 {
					values[i] = int64(random.Uint64())
				}
			case 3:
				values[i] = int64(random.Uint64())
			}
		}
		encoded := s.metadata.encodeResiduals(values)
		decoded, err := s.metadata.decodeResiduals(encoded, count)
		if err != nil {
			t.Fatal(err)
		}
		for i := range values {
			if values[i] != decoded[i] {
				t.Fatal("residual changed")
			}
		}
	}
}
