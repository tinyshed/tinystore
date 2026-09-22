package metrics

import (
	"bytes"
	"encoding/hex"
	"testing"

	"github.com/tinyshed/tinystore/codec"
)

func TestDirectoryVersionOneStillReads(t *testing.T) {
	const vector = "0101010000000000000000000000000000000000000000000000ef00000000000000f000080000000000000045400000000000004540000000000000454000000000000045400000000000b0c340000000000000000000000101040000897afcf1e179247e"
	data, err := hex.DecodeString(vector)
	if err != nil {
		t.Fatal(err)
	}
	group, err := decodeDirectory(7, 0, 239, data)
	if err != nil {
		t.Fatal(err)
	}
	if len(group.blocks) != 1 || group.blocks[0].summary.sum != 10080 || !group.blocks[0].summary.valid {
		t.Fatal("summary changed")
	}
	encoded, err := encodeDirectory(group)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(encoded, data) {
		t.Fatal("directory version one changed")
	}
	decoder, err := codec.New()
	if err != nil {
		t.Fatal(err)
	}
	defer decoder.Close()
	iterator, err := decoder.Decode(group.blocks[0].head, group.blocks[0].body)
	if err != nil {
		t.Fatal(err)
	}
	n := 0
	for iterator.Next() {
		point := iterator.Sample()
		if point.At != int64(n) || point.Value != 42 {
			t.Fatal("historical value changed")
		}
		n++
	}
	if iterator.Err() != nil || n != 240 {
		t.Fatal("historical block truncated", iterator.Err())
	}
}
