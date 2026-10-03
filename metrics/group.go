package metrics

import (
	"encoding/binary"
	"hash/crc32"
	"math"

	"github.com/tinyshed/tinystore/codec"
)

const (
	blockSamples         = codec.MaxSamples
	groupSlots           = 32
	inlineBytes          = 16
	maxDirectoryBytes    = 8192
	maxPayloadBytes      = 8200
	maxExpandedDirectory = maxDirectoryBytes - 128

	// directoryVersion is the first byte of every group directory, research's design/format.md
	directoryVersion = 4
)

type storedBlock struct {
	payload    int64
	clock      []byte
	head       codec.Head
	summary    blockSummary
	body       []byte
	bodyBytes  int
	summarized bool // this query needs only the checked exact summary
}

type blockGroup struct {
	clockID              int64
	clockBody            []byte
	modelScale           int
	seriesID, start, end int64
	live, allocation     uint32
	blocks               []storedBlock
}

func slotsMask(count int) uint32 {
	if count == groupSlots {
		return math.MaxUint32
	}
	return uint32(1)<<count - 1
}

func (g blockGroup) isLive(slot int) bool {
	return g.live&(uint32(1)<<slot) != 0
}

func (g blockGroup) isExternal(slot int) bool {
	return g.allocation&(uint32(1)<<slot) != 0
}

// checksum binds a directory to its series and its time bounds, so that it
// cannot be read as another group's.
func (g blockGroup) checksum(data []byte) uint32 {
	key := appendSigned64(nil, g.seriesID)
	key = appendSigned64(key, g.start)
	key = appendSigned64(key, g.end)
	return crc32.Update(crc32.ChecksumIEEE(key), crc32.IEEETable, data)
}

// appendSigned64 and signed64 carry an identifier's or a timestamp's bits as
// they are, eight bytes little endian.
func appendSigned64(out []byte, value int64) []byte {
	return binary.LittleEndian.AppendUint64(out, uint64(value)) //nolint:gosec // bits kept, not a value converted
}

func signed64(data []byte) int64 {
	return int64(binary.LittleEndian.Uint64(data)) //nolint:gosec // bits kept, not a value converted
}

// appendID writes a positive identifier as a varint.
func appendID(out []byte, id int64) []byte {
	return binary.AppendUvarint(out, uint64(id)) //nolint:gosec // identifiers are positive
}
