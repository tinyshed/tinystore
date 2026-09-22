package metrics

import (
	"bytes"
	"encoding/binary"
	"fmt"
	"hash/crc32"
	"io"
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
	format    byte
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

type directoryHeader struct {
	Version, Slots   uint8
	Live, Allocation uint32
	FirstPayload     int64
}

// floats travel as bits; SQLite REAL cannot preserve negative zero or NaN payloads
type directoryBlock struct {
	Start, End                                                   int64
	Count, BodyBytes                                             uint16
	FirstBits, LastBits, MinBits, MaxBits, SumBits, IncreaseBits uint64
	Resets                                                       uint16
	SummaryValid                                                 uint8
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
	return g.firstPayload + int64(bits.OnesCount32(g.allocation&slotsMask(slot)))
}

func (g blockGroup) checksum(data []byte) uint32 {
	key := binary.LittleEndian.AppendUint64(nil, uint64(g.seriesID)) //nolint:gosec // preserve identifier bits
	key = binary.LittleEndian.AppendUint64(key, uint64(g.start))     //nolint:gosec // preserve signed timestamp bits
	key = binary.LittleEndian.AppendUint64(key, uint64(g.end))       //nolint:gosec // preserve signed timestamp bits
	return crc32.Update(crc32.ChecksumIEEE(key), crc32.IEEETable, data)
}

func encodeDirectory(group blockGroup) ([]byte, error) {
	var out bytes.Buffer
	header := directoryHeader{Version: 1, Slots: uint8(len(group.blocks)), Live: group.live, Allocation: group.allocation, FirstPayload: group.firstPayload} //nolint:gosec // groups contain at most 32 blocks
	if err := binary.Write(&out, binary.LittleEndian, header); err != nil {
		return nil, fmt.Errorf("encode directory header: %w", err)
	}
	for slot, block := range group.blocks {
		summary := block.summary
		record := directoryBlock{Start: block.head.Start, End: block.head.End, Count: uint16(block.head.Count), BodyBytes: uint16(block.bodyBytes), //nolint:gosec // encoder bounds sample and body counts
			FirstBits: math.Float64bits(block.head.First), LastBits: math.Float64bits(summary.last), MinBits: math.Float64bits(summary.min), MaxBits: math.Float64bits(summary.max), SumBits: math.Float64bits(summary.sum), IncreaseBits: math.Float64bits(summary.increase), Resets: summary.resets}
		if summary.valid {
			record.SummaryValid = 1
		}
		if err := binary.Write(&out, binary.LittleEndian, record); err != nil {
			return nil, fmt.Errorf("encode block descriptor: %w", err)
		}
		if !group.isExternal(slot) {
			if _, err := out.Write(block.body); err != nil {
				return nil, fmt.Errorf("encode inline body: %w", err)
			}
		}
	}
	data := out.Bytes()
	if len(data)+4 > maxDirectoryBytes {
		return nil, fmt.Errorf("%w: directory bytes", ErrLimit)
	}
	return binary.LittleEndian.AppendUint32(data, group.checksum(data)), nil
}

func decodeDirectory(seriesID, start, end int64, data []byte) (blockGroup, error) {
	group := blockGroup{seriesID: seriesID, start: start, end: end}
	if len(data) < binary.Size(directoryHeader{})+4 || len(data) > maxDirectoryBytes {
		return group, fmt.Errorf("%w: directory size", ErrCorrupt)
	}
	content := data[:len(data)-4]
	if group.checksum(content) != binary.LittleEndian.Uint32(data[len(data)-4:]) {
		return group, fmt.Errorf("%w: directory checksum", ErrCorrupt)
	}
	reader := bytes.NewReader(content)
	var header directoryHeader
	if err := binary.Read(reader, binary.LittleEndian, &header); err != nil {
		return group, fmt.Errorf("%w: directory header: %w", ErrCorrupt, err)
	}
	if header.Version != 1 || header.Slots < 1 || header.Slots > groupSlots {
		return group, fmt.Errorf("%w: directory version or slots", ErrCorrupt)
	}
	allowed := slotsMask(int(header.Slots))
	if header.Live == 0 || header.Live & ^allowed != 0 || header.Allocation & ^allowed != 0 {
		return group, fmt.Errorf("%w: directory masks", ErrCorrupt)
	}
	allocated := bits.OnesCount32(header.Allocation)
	if (allocated == 0 && header.FirstPayload != 0) || (allocated > 0 && (header.FirstPayload < 1 || header.FirstPayload > math.MaxInt64-int64(allocated))) {
		return group, fmt.Errorf("%w: payload identifiers", ErrCorrupt)
	}
	group.live, group.allocation, group.firstPayload = header.Live, header.Allocation, header.FirstPayload
	for slot := range int(header.Slots) {
		var record directoryBlock
		if err := binary.Read(reader, binary.LittleEndian, &record); err != nil {
			return group, fmt.Errorf("%w: block descriptor: %w", ErrCorrupt, err)
		}
		if record.Count < 1 || record.Count > blockSamples || record.End < record.Start || record.End == math.MaxInt64 || record.BodyBytes < 8 || record.BodyBytes > maxPayloadBytes || record.SummaryValid > 1 || record.Resets >= record.Count {
			return group, fmt.Errorf("%w: block descriptor bounds", ErrCorrupt)
		}
		if (record.Count == 1 && record.Start != record.End) || (slot > 0 && record.Start <= group.blocks[slot-1].head.End) {
			return group, fmt.Errorf("%w: block ordering", ErrCorrupt)
		}
		block := storedBlock{
			head: codec.Head{Start: record.Start, End: record.End, Count: int(record.Count), First: math.Float64frombits(record.FirstBits)}, bodyBytes: int(record.BodyBytes),
			summary: blockSummary{last: math.Float64frombits(record.LastBits), min: math.Float64frombits(record.MinBits), max: math.Float64frombits(record.MaxBits), sum: math.Float64frombits(record.SumBits), increase: math.Float64frombits(record.IncreaseBits), resets: record.Resets, valid: record.SummaryValid == 1},
		}
		if !group.isExternal(slot) {
			if block.bodyBytes > inlineBytes {
				return group, fmt.Errorf("%w: inline body size", ErrCorrupt)
			}
			block.body = make([]byte, block.bodyBytes)
			if _, err := io.ReadFull(reader, block.body); err != nil {
				return group, fmt.Errorf("%w: inline body: %w", ErrCorrupt, err)
			}
		}
		group.blocks = append(group.blocks, block)
	}
	if reader.Len() != 0 || group.blocks[0].head.Start != start || group.blocks[len(group.blocks)-1].head.End != end {
		return group, fmt.Errorf("%w: directory extent", ErrCorrupt)
	}
	return group, nil
}
