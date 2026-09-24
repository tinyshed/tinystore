package spike

import (
	"encoding/binary"
	"fmt"
	"math"
	"os"
	"slices"
	"sort"
	"testing"

	"github.com/klauspost/compress/huff0"
	"github.com/klauspost/compress/zstd"

	"github.com/tinyshed/tinystore/codec"
)

// monotoneInt maps a float onto the integers in the same order, so a residual
// against a prediction is a distance in units in the last place
func monotoneInt(v float64) int64 {
	u := math.Float64bits(v)
	if u&(1<<63) != 0 {
		u = ^u
	} else {
		u |= 1 << 63
	}
	return int64(u)
}

func differences(values []int64) []int64 {
	out := make([]int64, len(values)-1)
	for i := range out {
		out[i] = values[i+1] - values[i]
	}
	return out
}

func varints(symbols []int64) []byte {
	out := make([]byte, 0, len(symbols)*2)
	for _, s := range symbols {
		out = binary.AppendUvarint(out, uint64(s)<<1^uint64(s>>63))
	}
	return out
}

type modelTotals struct {
	samples                                    int
	current, varint, huffOwn, huffShared       int
	zstdBlock, zstdDict, zstdFamily, tableCost int
	bestWithShared, sharedWins                 int
}

// TestSharedModelsAgainstCrossSeriesPrediction asks two questions on the same
// blocks: whether one instance of a field predicts another, and whether the
// instances of a field should at least share one entropy model
func TestSharedModelsAgainstCrossSeriesPrediction(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure")
	}
	series := readJSONLCorpus(t)
	c, err := codec.New()
	if err != nil {
		t.Fatal(err)
	}
	defer c.Close()
	plain, err := zstd.NewWriter(nil, zstd.WithEncoderConcurrency(1),
		zstd.WithWindowSize(1<<16), zstd.WithLowerEncoderMem(true))
	if err != nil {
		t.Fatal(err)
	}
	defer plain.Close()

	families := map[string][]corpusSeries{}
	for _, s := range series {
		name := s.Metric["__name__"]
		families[name] = append(families[name], s)
	}
	names := make([]string, 0, len(families))
	for name := range families {
		names = append(names, name)
	}
	sort.Strings(names)

	var totals modelTotals
	misaligned := 0
	ownBits, dodBits, refBits, refDeltaBits := 0.0, 0.0, 0.0, 0.0
	correlationSum, correlationCount := 0.0, 0
	perFamily := make([]string, 0, len(names))

	for _, name := range names {
		instances := families[name]
		sort.Slice(instances, func(i, j int) bool {
			return instances[i].Metric["hostname"] < instances[j].Metric["hostname"]
		})
		// a real agent runs its inputs at their own intervals and a container's
		// series begins when the container does, so instances are compared on
		// the instants they actually share rather than skipped for ragged ends
		present := map[int64]int{}
		for _, s := range instances {
			seen := map[int64]bool{}
			for _, at := range s.Times {
				if !seen[at] {
					seen[at] = true
					present[at]++
				}
			}
		}
		common := make([]int64, 0, len(present))
		for at, n := range present {
			if n == len(instances) {
				common = append(common, at)
			}
		}
		slices.Sort(common)
		length := len(common)
		if length < researchBlock || len(instances) < 2 {
			misaligned++
			continue
		}
		position := make(map[int64]int, length)
		for i, at := range common {
			position[at] = i
		}
		values := make([][]int64, len(instances))
		for h, s := range instances {
			values[h] = make([]int64, length)
			for i, v := range s.Values {
				j, ok := position[s.Times[i]]
				if !ok {
					continue
				}
				if n, integral := integerOf(v); integral {
					values[h][j] = n
					continue
				}
				values[h][j] = monotoneInt(v)
			}
		}

		own, dod, ref, refDelta := map[int64]int{}, map[int64]int{}, map[int64]int{}, map[int64]int{}
		for h := range values {
			deltas := differences(values[h])
			for _, d := range deltas {
				own[d]++
			}
			for _, d := range differences(deltas) {
				dod[d]++
			}
			if h == 0 {
				continue
			}
			residual := make([]int64, length)
			for i := range residual {
				residual[i] = values[h][i] - values[0][i]
			}
			for _, r := range residual[1:] {
				ref[r]++
			}
			for _, d := range differences(residual) {
				refDelta[d]++
			}
			correlationSum += correlation(deltas, differences(values[0]))
			correlationCount++
		}
		ownH, _, ownN := alphabetEntropy(own)
		dodH, _, dodN := alphabetEntropy(dod)
		refH, _, refN := alphabetEntropy(ref)
		refDeltaH, _, refDeltaN := alphabetEntropy(refDelta)
		ownBits += ownH * float64(ownN)
		dodBits += dodH * float64(dodN)
		refBits += refH * float64(refN)
		refDeltaBits += refDeltaH * float64(refDeltaN)

		// the bytes half: the same own-delta stream through five packers
		var blocks [][]byte
		var currentBlock []int
		family := make([]byte, 0, 1<<20)
		for h := range instances {
			for start := 0; start < length; start += researchBlock {
				end := min(start+researchBlock, length)
				_, body, encodeErr := c.Encode(onCommonClock(common[start:end], values[h][start:end]))
				if encodeErr != nil {
					t.Fatal(encodeErr)
				}
				flat := make([]codec.Sample, end-start)
				for i := range flat {
					flat[i] = codec.Sample{At: common[start+i], Value: 1}
				}
				_, floor, floorErr := c.Encode(flat)
				if floorErr != nil {
					t.Fatal(floorErr)
				}
				totals.current += len(body) - len(floor)
				currentBlock = append(currentBlock, len(body)-len(floor))
				totals.samples += end - start
				stream := varints(differences(values[h][start:end]))
				blocks = append(blocks, stream)
				family = append(family, stream...)
			}
		}
		var scratch huff0.Scratch
		scratch.Reuse = huff0.ReusePolicyNone
		training := family
		if len(training) > 100<<10 {
			training = training[:100<<10]
		}
		withTable, _, trainErr := huff0.Compress1X(training, &scratch)
		tableBytes := 0
		shared := trainErr == nil
		if shared {
			scratch.Reuse = huff0.ReusePolicyMust
			withoutTable, reused, reuseErr := huff0.Compress1X(training, &scratch)
			shared = reuseErr == nil && reused
			if shared {
				tableBytes = len(withTable) - len(withoutTable)
			}
		}
		dictionary, dictErr := zstd.BuildDict(zstd.BuildDictOptions{ID: 1, Contents: blocks})
		var dictWriter *zstd.Encoder
		if dictErr == nil {
			dictWriter, dictErr = zstd.NewWriter(nil, zstd.WithEncoderConcurrency(1),
				zstd.WithEncoderDict(dictionary), zstd.WithWindowSize(1<<16), zstd.WithLowerEncoderMem(true))
		}
		familyVarint, familyHuffOwn, familyHuffShared := 0, 0, 0
		familyZstdBlock, familyZstdDict := 0, 0
		for index, stream := range blocks {
			familyVarint += len(stream)
			var per huff0.Scratch
			per.Reuse = huff0.ReusePolicyNone
			if out, _, huffErr := huff0.Compress1X(stream, &per); huffErr == nil && len(out) < len(stream) {
				familyHuffOwn += len(out)
			} else {
				familyHuffOwn += len(stream)
			}
			sharedSize := len(stream)
			if shared {
				scratch.Reuse = huff0.ReusePolicyMust
				out, reused, huffErr := huff0.Compress1X(stream, &scratch)
				if huffErr == nil && reused && len(out) < sharedSize {
					sharedSize = len(out)
				}
			}
			familyHuffShared += sharedSize
			if sharedSize < currentBlock[index] {
				totals.sharedWins++
				totals.bestWithShared += sharedSize
			} else {
				totals.bestWithShared += currentBlock[index]
			}
			familyZstdBlock += min(len(plain.EncodeAll(stream, nil)), len(stream))
			if dictErr == nil {
				familyZstdDict += min(len(dictWriter.EncodeAll(stream, nil)), len(stream))
			} else {
				familyZstdDict += len(stream)
			}
		}
		if dictWriter != nil {
			dictWriter.Close()
		}
		familyStream := len(plain.EncodeAll(family, nil))
		totals.varint += familyVarint
		totals.huffOwn += familyHuffOwn
		totals.huffShared += familyHuffShared + tableBytes
		totals.tableCost += tableBytes
		totals.zstdBlock += familyZstdBlock
		totals.zstdDict += familyZstdDict + len(dictionary)
		totals.zstdFamily += familyStream
		if name == "cpu_usage_user" || name == "mem_used" || name == "diskio_reads" || name == "mem_used_percent" {
			perFamily = append(perFamily, familyLine(name, len(instances)*length,
				familyVarint, familyHuffOwn, familyHuffShared+tableBytes, familyZstdBlock,
				familyZstdDict+len(dictionary), familyStream, tableBytes, len(dictionary)))
		}
	}

	n := float64(totals.samples)
	t.Logf("SAMPLES %d, %d field families skipped for not sharing one clock", totals.samples, misaligned)
	t.Logf("PREDICTION bits a sample, order-0 over the whole field family:")
	t.Logf("   own delta            %.3f", ownBits/n)
	t.Logf("   own delta of delta   %.3f", dodBits/n)
	t.Logf("   residual vs host_0   %.3f", refBits/n)
	t.Logf("   its delta            %.3f", refDeltaBits/n)
	t.Logf("   mean correlation of one host's deltas with host_0's: %.4f over %d pairs",
		correlationSum/float64(correlationCount), correlationCount)
	t.Logf("VALUE BYTES a sample, the same own-delta stream through five packers:")
	t.Logf("   current codec, values only   %.4f", float64(totals.current)/n)
	t.Logf("   zigzag varints               %.4f", float64(totals.varint)/n)
	t.Logf("   huff0, a table per block     %.4f", float64(totals.huffOwn)/n)
	t.Logf("   huff0, one table per family  %.4f  (tables %d B)", float64(totals.huffShared)/n, totals.tableCost)
	t.Logf("   zstd per block               %.4f", float64(totals.zstdBlock)/n)
	t.Logf("   zstd, dictionary per family  %.4f", float64(totals.zstdDict)/n)
	t.Logf("   zstd, one stream per family  %.4f  (no random access)", float64(totals.zstdFamily)/n)
	t.Logf("   current, or the shared table when it is smaller  %.4f  (shared chosen in %d blocks)",
		float64(totals.bestWithShared+totals.tableCost)/n, totals.sharedWins)
	for _, line := range perFamily {
		t.Log(line)
	}
}

func familyLine(name string, samples, varint, huffOwn, huffShared, zstdBlock, zstdDict, zstdFamily, table, dict int) string {
	f := func(v int) float64 { return float64(v) / float64(samples) }
	return sprintf("   %-22s varint=%.3f huffBlock=%.3f huffShared=%.3f zstdBlock=%.3f zstdDict=%.3f zstdStream=%.3f table=%dB dict=%dB",
		name, f(varint), f(huffOwn), f(huffShared), f(zstdBlock), f(zstdDict), f(zstdFamily), table, dict)
}

// onCommonClock hands the packers the integers the family is compared on, so a
// float family and an integer family go through one transform
func onCommonClock(times, values []int64) []codec.Sample {
	out := make([]codec.Sample, len(times))
	for i := range out {
		out[i] = codec.Sample{At: times[i], Value: float64(values[i])}
	}
	return out
}

func correlation(a, b []int64) float64 {
	if len(a) != len(b) || len(a) == 0 {
		return 0
	}
	var sumA, sumB float64
	for i := range a {
		sumA += float64(a[i])
		sumB += float64(b[i])
	}
	meanA, meanB := sumA/float64(len(a)), sumB/float64(len(b))
	var cov, varA, varB float64
	for i := range a {
		da, db := float64(a[i])-meanA, float64(b[i])-meanB
		cov += da * db
		varA += da * da
		varB += db * db
	}
	if varA == 0 || varB == 0 {
		return 0
	}
	return cov / math.Sqrt(varA*varB)
}

func sprintf(format string, args ...any) string { return fmt.Sprintf(format, args...) }
