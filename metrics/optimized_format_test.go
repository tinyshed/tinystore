package metrics

import (
	"bytes"
	"encoding/hex"
	"testing"
)

func TestVersionTwoDirectoryAndClockRemainReadable(t *testing.T) {
	const clockVector = "010100ef01f00100b9a118b1"
	const directoryVector = "020101000000000000000000000000000000010000000000000000d0023f0000325ad605"
	s := encodingStore(t)
	clock, err := hex.DecodeString(clockVector)
	if err != nil {
		t.Fatal(err)
	}
	directory, err := hex.DecodeString(directoryVector)
	if err != nil {
		t.Fatal(err)
	}
	blocks, err := decodeClockGroup(clock)
	if err != nil {
		t.Fatal(err)
	}
	group, err := s.readDirectory(7, 0, 239, 1, directory, blocks)
	if err != nil {
		t.Fatal(err)
	}
	if len(group.blocks) != 1 || len(group.blocks[0].body) != 0 || group.blocks[0].summary.sum != 10080 {
		t.Fatal("constant summary changed")
	}
	encoded, err := s.writeDirectory(group)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(encoded, directory) {
		t.Fatal("version two changed")
	}
	points, err := s.decodeBlock(group.blocks[0])
	if err != nil {
		t.Fatal(err)
	}
	if len(points) != 240 {
		t.Fatal("count changed")
	}
	for i, point := range points {
		if point.At != int64(i) || point.Value != 42 {
			t.Fatal("sample changed")
		}
	}
}
