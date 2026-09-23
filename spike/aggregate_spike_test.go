package spike

import (
	"bufio"
	"encoding/json"
	"math"
	"math/big"
	"os"
	"testing"
)

func exactFloatUnits(value float64, into *big.Int) bool {
	bits := math.Float64bits(value)
	exponent := int((bits >> 52) & 0x7ff)
	if exponent == 0x7ff {
		return false
	}
	mantissa := bits & ((1 << 52) - 1)
	if exponent != 0 {
		mantissa |= 1 << 52
	}
	into.SetUint64(mantissa)
	if exponent != 0 {
		into.Lsh(into, uint(exponent-1))
	}
	if bits>>63 != 0 {
		into.Neg(into)
	}
	return true
}

func growExpansion(terms []float64, value float64) ([]float64, bool) {
	carry := value
	var next []float64
	for _, term := range terms {
		sum := carry + term
		if math.IsInf(sum, 0) {
			return nil, false
		}
		back := sum - carry
		errorTerm := carry - (sum - back) + (term - back)
		if errorTerm != 0 {
			next = append(next, errorTerm)
		}
		carry = sum
	}
	return append(next, carry), true
}

func exactSummaryBytes(total *big.Int) int {
	if total.Sign() == 0 {
		return 1
	}
	magnitude := new(big.Int).Abs(total)
	shift := magnitude.TrailingZeroBits()
	magnitude.Rsh(magnitude, shift)
	return 1 + 2 + len(magnitude.Bytes())
}

func TestAggregateRepresentationSpike(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") != "1" {
		t.Skip("set TINYSTORE_SPIKE=1")
	}
	path := os.Getenv("TINYSTORE_JSONL")
	if path == "" {
		t.Skip("set TINYSTORE_JSONL to a normalized corpus")
	}
	file, err := os.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	defer file.Close()
	scanner := bufio.NewScanner(file)
	scanner.Buffer(make([]byte, 1<<20), 1<<28)
	var blocks, samples, nonFinite, expansionFailures int
	var variableBytes, fixedBytes, expansionBytes, maxTerms int
	term := new(big.Int)
	fromExpansion := new(big.Int)
	for scanner.Scan() {
		var row struct {
			Values []float64 `json:"values"`
		}
		if err := json.Unmarshal(scanner.Bytes(), &row); err != nil {
			t.Fatal(err)
		}
		for first := 0; first < len(row.Values); first += 240 {
			last := min(first+240, len(row.Values))
			total := new(big.Int)
			var expansion []float64
			valid, expansionValid := true, true
			for _, value := range row.Values[first:last] {
				if !exactFloatUnits(value, term) {
					nonFinite++
					valid = false
					continue
				}
				total.Add(total, term)
				if expansionValid {
					expansion, expansionValid = growExpansion(expansion, value)
				}
			}
			samples += last - first
			if !valid {
				continue
			}
			blocks++
			variableBytes += exactSummaryBytes(total)
			fixedBytes += 272
			if !expansionValid {
				expansionFailures++
				continue
			}
			fromExpansion.SetInt64(0)
			for _, value := range expansion {
				if !exactFloatUnits(value, term) {
					t.Fatal("nonfinite expansion component")
				}
				fromExpansion.Add(fromExpansion, term)
			}
			if fromExpansion.Cmp(total) != 0 {
				t.Fatal("expansion lost exact sum")
			}
			maxTerms = max(maxTerms, len(expansion))
			expansionBytes += 1 + 8*len(expansion)
		}
	}
	if err := scanner.Err(); err != nil {
		t.Fatal(err)
	}
	if blocks == 0 {
		t.Fatal("empty corpus")
	}
	t.Logf("AGGREGATE corpus=%s blocks=%d samples=%d nonfinite=%d variable_bytes=%d fixed_bytes=%d expansion_bytes=%d expansion_failures=%d max_expansion_terms=%d", path, blocks, samples, nonFinite, variableBytes, fixedBytes, expansionBytes, expansionFailures, maxTerms)
	vector := []float64{1e16, 1, -1e16, 1}
	exact := new(big.Int)
	for _, value := range vector {
		exactFloatUnits(value, term)
		exact.Add(exact, term)
	}
	result := new(big.Float).SetPrec(53).SetMode(big.ToNearestEven).SetInt(exact)
	result.SetMantExp(result, -1074)
	rounded, _ := result.Float64()
	if rounded != 2 {
		t.Fatalf("correctly rounded exact sum: %v", rounded)
	}
	var unsafeExpansion []float64
	for _, value := range []float64{math.MaxFloat64, math.MaxFloat64, -math.MaxFloat64, -math.MaxFloat64} {
		var ok bool
		unsafeExpansion, ok = growExpansion(unsafeExpansion, value)
		if !ok {
			return
		}
	}
	t.Fatal("overflowing expansion did not report its failure")
}
