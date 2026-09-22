//nolint:gosec // descriptor counts and masks are checked before conversion; identities retain their signed bits
package metrics

import (
	"encoding/binary"
	"fmt"
	"math"
	"math/bits"
)

func (s *Store) writeDirectory(group blockGroup) ([]byte, error) {
	if group.format < 2 {
		return encodeDirectory(group)
	}
	var raw []byte
	for slot, block := range group.blocks {
		raw = appendFloat(raw, block.head.First)
		summary := block.summary
		values := []float64{summary.last, summary.min, summary.max, summary.sum, summary.increase}
		predictions := []float64{block.head.First, block.head.First, block.head.First, float64(block.head.Count) * block.head.First, 0}
		flags := byte(0)
		if summary.valid {
			flags |= 1 << 5
		}
		for i, value := range values {
			if math.Float64bits(value) == math.Float64bits(predictions[i]) {
				flags |= 1 << uint(i)
			}
		}
		raw = append(raw, flags)
		for i, value := range values {
			if flags&(1<<uint(i)) == 0 {
				raw = appendFloat(raw, value)
			}
		}
		raw = binary.AppendUvarint(raw, uint64(summary.resets))
		raw = binary.AppendUvarint(raw, uint64(block.bodyBytes))
		if group.format >= 3 && group.isExternal(slot) {
			raw = binary.AppendUvarint(raw, uint64(block.payload))
		} else if !group.isExternal(slot) {
			raw = append(raw, block.body...)
		}
	}
	if len(raw) > maxDirectoryBytes {
		return nil, fmt.Errorf("%w: expanded directory", ErrLimit)
	}
	header := []byte{group.format, byte(len(group.blocks))}
	header = binary.LittleEndian.AppendUint32(header, group.live)
	header = binary.LittleEndian.AppendUint32(header, group.allocation)
	header = binary.LittleEndian.AppendUint64(header, uint64(group.firstPayload))
	header = binary.LittleEndian.AppendUint64(header, uint64(group.clockID))
	header = append(header, s.metadata.encode(raw)...)
	if len(header)+4 > maxDirectoryBytes {
		return nil, fmt.Errorf("%w: directory size", ErrLimit)
	}
	return binary.LittleEndian.AppendUint32(header, group.checksum(header)), nil
}

func (s *Store) readDirectory(id, start, end, clockID int64, data []byte, clock []storedBlock) (blockGroup, error) {
	if len(data) > 0 && data[0] == 1 {
		if clockID != 0 {
			return blockGroup{}, fmt.Errorf("%w: legacy clock reference", ErrCorrupt)
		}
		return decodeDirectory(id, start, end, data)
	}
	group := blockGroup{format: 2, seriesID: id, start: start, end: end, clockID: clockID}
	if len(data) < 31 || len(data) > maxDirectoryBytes || (data[0] != 2 && data[0] != 3) {
		return group, fmt.Errorf("%w: directory version or size", ErrCorrupt)
	}
	group.format = data[0]
	content := data[:len(data)-4]
	if group.checksum(content) != binary.LittleEndian.Uint32(data[len(data)-4:]) {
		return group, fmt.Errorf("%w: directory checksum", ErrCorrupt)
	}
	count := int(data[1])
	if count < 1 || count > groupSlots || len(clock) != count {
		return group, fmt.Errorf("%w: directory clock slots", ErrCorrupt)
	}
	group.live = binary.LittleEndian.Uint32(data[2:])
	group.allocation = binary.LittleEndian.Uint32(data[6:])
	group.firstPayload = int64(binary.LittleEndian.Uint64(data[10:]))
	storedClock := int64(binary.LittleEndian.Uint64(data[18:]))
	if clockID <= 0 || storedClock != clockID || group.live == 0 || group.live & ^slotsMask(count) != 0 || group.allocation & ^slotsMask(count) != 0 {
		return group, fmt.Errorf("%w: group references or masks", ErrCorrupt)
	}
	allocated := bits.OnesCount32(group.allocation)
	if (group.format == 3 && group.firstPayload != 0) ||
		(group.format == 2 && ((allocated == 0 && group.firstPayload != 0) || (allocated > 0 && (group.firstPayload < 1 || group.firstPayload > math.MaxInt64-int64(allocated))))) {
		return group, fmt.Errorf("%w: group payload range", ErrCorrupt)
	}
	plain, err := s.metadata.decode(content[26:])
	if err != nil {
		return group, err
	}
	reader := binaryReader{data: plain}
	for slot := range count {
		block := clock[slot]
		block.format = 2
		block.head.First = reader.float()
		flags := reader.byte()
		if flags&0xc0 != 0 {
			return group, fmt.Errorf("%w: summary flags", ErrCorrupt)
		}
		values := []float64{block.head.First, block.head.First, block.head.First, float64(block.head.Count) * block.head.First, 0}
		for i := range values {
			if flags&(1<<uint(i)) == 0 {
				values[i] = reader.float()
			}
		}
		resets := reader.size(block.head.Count - 1)
		block.summary = blockSummary{last: values[0], min: values[1], max: values[2], sum: values[3], increase: values[4], resets: uint16(resets), valid: flags&(1<<5) != 0}
		block.bodyBytes = reader.size(maxPayloadBytes)
		if block.bodyBytes > 0 && block.bodyBytes < 6 {
			return group, fmt.Errorf("%w: value body length", ErrCorrupt)
		}
		if group.isExternal(slot) {
			if block.bodyBytes <= inlineBytes {
				return group, fmt.Errorf("%w: external body length", ErrCorrupt)
			}
			if group.format == 3 {
				id := reader.unsigned()
				if id == 0 || id >= math.MaxInt64 {
					return group, fmt.Errorf("%w: external payload identifier", ErrCorrupt)
				}
				block.payload = int64(id)
				for _, previous := range group.blocks {
					if previous.payload == block.payload {
						return group, fmt.Errorf("%w: repeated payload identifier", ErrCorrupt)
					}
				}
			}
		} else {
			if block.bodyBytes > inlineBytes {
				return group, fmt.Errorf("%w: inline body length", ErrCorrupt)
			}
			block.body = reader.take(block.bodyBytes)
		}
		group.blocks = append(group.blocks, block)
	}
	if err = reader.finish(); err != nil {
		return group, err
	}
	if group.blocks[0].head.Start != start || group.blocks[count-1].head.End != end {
		return group, fmt.Errorf("%w: group temporal extent", ErrCorrupt)
	}
	return group, nil
}
