package metrics

import (
	"encoding/binary"
	"fmt"
	"math"

	"github.com/tinyshed/tinystore/codec"
)

func (s *Store) writeDirectory(group blockGroup) ([]byte, error) {
	var raw []byte
	for slot, block := range group.blocks {
		raw = group.appendBlock(raw, slot, block)
	}
	if len(raw) > maxDirectoryBytes {
		return nil, fmt.Errorf("%w: expanded directory", ErrLimit)
	}
	header := []byte{directoryVersion, byte(len(group.blocks))} //nolint:gosec // a group holds at most 32 blocks
	header = binary.LittleEndian.AppendUint32(header, group.live)
	header = binary.LittleEndian.AppendUint32(header, group.allocation)
	header = appendSigned64(header, group.clockID)
	header = append(header, s.metadata.encode(raw)...)
	if len(header)+4 > maxDirectoryBytes {
		return nil, fmt.Errorf("%w: directory size", ErrLimit)
	}
	return binary.LittleEndian.AppendUint32(header, group.checksum(header)), nil
}

// appendBlock writes one block's first value, its summary without the values
// that equal their prediction, its exact sums when it has them, and where its
// body lives: inline, or the payload its id names.
func (g blockGroup) appendBlock(raw []byte, slot int, block storedBlock) []byte {
	raw = appendFloat(raw, block.head.First)
	summary := block.summary
	values := []float64{summary.last, summary.min, summary.max, summary.sum, summary.increase}
	flags := byte(0)
	if summary.valid {
		flags |= 1 << 5
	}
	if summary.exactSum != nil {
		flags |= 1 << 6
	}
	for i, prediction := range summaryPredictions(block.head) {
		if math.Float64bits(values[i]) == math.Float64bits(prediction) {
			flags |= 1 << i
		}
	}
	raw = append(raw, flags)
	for i, value := range values {
		if flags&(1<<i) == 0 {
			raw = appendFloat(raw, value)
		}
	}
	raw = appendCount(raw, int(summary.resets))
	if flags&(1<<6) != 0 {
		raw = append(raw, summary.exactSum...)
		raw = append(raw, summary.exactIncrease...)
	}
	raw = appendCount(raw, block.bodyBytes)
	if g.isExternal(slot) {
		return appendID(raw, block.payload)
	}
	return append(raw, block.body...)
}

// summaryPredictions is what a summary holds when every sample equals the
// first; a value that matches its prediction is not stored:
//
//	first 42, count 240    last 42, min 42, max 42, sum 10080, increase 0
func summaryPredictions(head codec.Head) []float64 {
	first := head.First
	return []float64{first, first, first, float64(head.Count) * first, 0}
}

// readDirectory checks a group directory against its series, its time bounds
// and its clock, then reads each block's summary and where its body lives.
func (s *Store) readDirectory(id int64, row groupRow, clock []storedBlock) (blockGroup, error) {
	group, content, err := checkDirectory(id, row, len(clock))
	if err != nil {
		return group, err
	}

	plain, err := s.metadata.decode(content[directoryHeader:])
	if err != nil {
		return group, err
	}

	reader := binaryReader{data: plain}
	for slot := range clock {
		var block storedBlock
		if block, err = group.readBlock(&reader, slot, clock[slot]); err != nil {
			return group, err
		}
		group.blocks = append(group.blocks, block)
	}
	if err = reader.finish(); err != nil {
		return group, err
	}

	if group.blocks[0].head.Start != row.start || group.blocks[len(clock)-1].head.End != row.end {
		return group, fmt.Errorf("%w: group temporal extent", ErrCorrupt)
	}
	return group, nil
}

// directoryHeader is a directory's bytes before its descriptor stream: its
// version, slots, live and external masks and clock id
const directoryHeader = 18

// checkDirectory checks a directory's version, size, checksum and header, and
// returns the directory without its checksum.
func checkDirectory(id int64, row groupRow, slots int) (blockGroup, []byte, error) {
	group := blockGroup{seriesID: id, start: row.start, end: row.end, clockID: row.clockID}
	data := row.data
	if len(data) < directoryHeader+1+4 || len(data) > maxDirectoryBytes || data[0] != directoryVersion {
		return group, nil, fmt.Errorf("%w: directory version or size", ErrCorrupt)
	}
	content := data[:len(data)-4]
	if group.checksum(content) != binary.LittleEndian.Uint32(data[len(data)-4:]) {
		return group, nil, fmt.Errorf("%w: directory checksum", ErrCorrupt)
	}
	count := int(data[1])
	if count < 1 || count > groupSlots || slots != count {
		return group, nil, fmt.Errorf("%w: directory clock slots", ErrCorrupt)
	}
	group.live = binary.LittleEndian.Uint32(data[2:])
	group.allocation = binary.LittleEndian.Uint32(data[6:])
	if err := group.checkReferences(signed64(data[10:]), count); err != nil {
		return group, nil, err
	}
	return group, content, nil
}

// checkReferences refuses a directory bound to another clock, or with masks
// beyond its slots.
func (g blockGroup) checkReferences(storedClock int64, count int) error {
	beyond := ^slotsMask(count)
	if g.clockID <= 0 || storedClock != g.clockID || g.live == 0 || g.live&beyond != 0 || g.allocation&beyond != 0 {
		return fmt.Errorf("%w: group references or masks", ErrCorrupt)
	}
	return nil
}

// readBlock reads one block's first value, summary and body address; its
// timestamps come from the clock.
func (g *blockGroup) readBlock(reader *binaryReader, slot int, block storedBlock) (storedBlock, error) {
	block.head.First = reader.float()
	flags := reader.byte()
	if flags&0x80 != 0 {
		return block, fmt.Errorf("%w: summary flags", ErrCorrupt)
	}
	block.summary = readSummary(reader, block.head, flags)
	if flags&0x40 != 0 {
		block.summary.exactSum = readExact(reader)
		block.summary.exactIncrease = readExact(reader)
		if reader.err != nil {
			return block, reader.err
		}
	}
	block.bodyBytes = reader.size(maxPayloadBytes)
	if block.bodyBytes > 0 && block.bodyBytes < 6 {
		return block, fmt.Errorf("%w: value body length", ErrCorrupt)
	}
	if !g.isExternal(slot) {
		if block.bodyBytes > inlineBytes {
			return block, fmt.Errorf("%w: inline body length", ErrCorrupt)
		}
		block.body = reader.take(block.bodyBytes)
		return block, nil
	}
	if block.bodyBytes <= inlineBytes {
		return block, fmt.Errorf("%w: external body length", ErrCorrupt)
	}
	return g.readPayloadID(reader, block)
}

func readSummary(reader *binaryReader, head codec.Head, flags byte) blockSummary {
	values := summaryPredictions(head)
	for i := range values {
		if flags&(1<<i) == 0 {
			values[i] = reader.float()
		}
	}
	resets := reader.size(head.Count - 1)
	return blockSummary{
		last: values[0], min: values[1], max: values[2], sum: values[3], increase: values[4],
		resets: uint16(resets), //nolint:gosec // bounded by the block's 240 samples
		valid:  flags&(1<<5) != 0,
	}
}

// readPayloadID reads an external block's own payload address, which no
// earlier block of the group may share.
func (g *blockGroup) readPayloadID(reader *binaryReader, block storedBlock) (storedBlock, error) {
	id := reader.unsigned()
	if id == 0 || id >= math.MaxInt64 {
		return block, fmt.Errorf("%w: external payload identifier", ErrCorrupt)
	}
	block.payload = int64(id)
	for _, previous := range g.blocks {
		if previous.payload == block.payload {
			return block, fmt.Errorf("%w: repeated payload identifier", ErrCorrupt)
		}
	}
	return block, nil
}
