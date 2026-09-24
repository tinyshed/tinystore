package metrics

import (
	"encoding/binary"
	"hash/crc32"
	"math"
	"math/bits"

	"github.com/tinyshed/tinystore/codec"
)

const (
	blockSamples      = codec.MaxSamples
	groupSlots        = 32
	inlineBytes       = 16
	maxDirectoryBytes = 8192
	maxPayloadBytes   = 8200
)

type blockSummary struct {
	last, min, max, sum, increase float64
	resets                        uint16
	valid                         bool
}

type storedBlock struct {
	payload   int64
	clock     []byte
	head      codec.Head
	summary   blockSummary
	body      []byte
	bodyBytes int
}

type blockGroup struct {
	format                             byte
	clockID                            int64
	clockBody                          []byte
	modelScale                         int
	seriesID, start, end, firstPayload int64
	live, allocation                   uint32
	blocks                             []storedBlock
}

func summarize(points []Sample, kind Kind) blockSummary {
	summary := blockSummary{min: points[0].Value, max: points[0].Value, last: points[len(points)-1].Value, valid: true}
	for i, point := range points {
		value := point.Value
		if math.IsNaN(value) || math.IsInf(value, 0) {
			summary.valid = false
		}
		if kind == Gauge {
			summary.min = math.Min(summary.min, value)
			summary.max = math.Max(summary.max, value)
			summary.sum += value
		} else {
			if value < 0 {
				summary.valid = false
			}
			if i > 0 {
				previous := points[i-1].Value //nolint:gosec // guarded by i > 0
				if value < previous {
					summary.resets++
					summary.increase += value
				} else {
					summary.increase += value - previous
				}
			}
		}
	}
	if math.IsNaN(summary.sum) || math.IsInf(summary.sum, 0) || math.IsNaN(summary.increase) || math.IsInf(summary.increase, 0) {
		summary.valid = false
	}
	return summary
}

func slotsMask(count int) uint32 {
	if count == groupSlots {
		return math.MaxUint32
	}
	return (uint32(1) << uint(count)) - 1 //nolint:gosec // callers validate slot counts in 0..32
}

func (g blockGroup) isLive(slot int) bool     { return g.live&(uint32(1)<<uint(slot)) != 0 }       //nolint:gosec // slot belongs to a checked group
func (g blockGroup) isExternal(slot int) bool { return g.allocation&(uint32(1)<<uint(slot)) != 0 } //nolint:gosec // slot belongs to a checked group

func (g blockGroup) payloadID(slot int) int64 {
	if g.format >= 3 {
		return g.blocks[slot].payload
	}
	return g.firstPayload + int64(bits.OnesCount32(g.allocation&slotsMask(slot)))
}

func (g blockGroup) checksum(data []byte) uint32 {
	key := binary.LittleEndian.AppendUint64(nil, uint64(g.seriesID)) //nolint:gosec // preserve identifier bits
	key = binary.LittleEndian.AppendUint64(key, uint64(g.start))     //nolint:gosec // preserve signed timestamp bits
	key = binary.LittleEndian.AppendUint64(key, uint64(g.end))       //nolint:gosec // preserve signed timestamp bits
	return crc32.Update(crc32.ChecksumIEEE(key), crc32.IEEETable, data)
}
