package wire_test

import (
	"reflect"
	"testing"

	"github.com/tinyshed/tinystore/server/wire"
)

func TestBlobsMessagesReadBackAsTheyWereWritten(t *testing.T) {
	jpeg := "image/jpeg"
	messages := []struct {
		written interface{ Append([]byte) []byte }
		read    interface{ Decode([]byte) error }
	}{
		{wire.BlobsBucket{Name: "avatars", DefaultTTL: 60_000, MaxSize: 20 << 20}, &wire.BlobsBucket{}},
		{wire.BlobsCall{
			Handle: 1, Owners: []string{"users", "7"}, Key: "a/b.jpg", To: "c.jpg", ContentType: &jpeg,
			Meta: map[string]string{"width": "640"}, TTL: 1000, ExpireAt: 1_790_000_000_000, Size: 300,
			IfMatch: `"9f86"`, IfNoneMatch: true, Prefix: "a/", After: "a/a", Limit: 10, Offset: 5, Length: 7,
		}, &wire.BlobsCall{}},
		{wire.BlobsCall{Handle: 2, Size: -1}, &wire.BlobsCall{}},
		{wire.BlobsObject{
			Found: true, Key: "b.jpg", Size: 3, ETag: `"00"`, ContentType: "image/png", Modified: 1, Expires: 2,
			Meta: map[string]string{"name": "cat.png"},
		}, &wire.BlobsObject{}},
		{wire.BlobsTotal{Objects: 3, Bytes: 1 << 40}, &wire.BlobsTotal{}},
		{wire.BlobsPage{More: true, After: "k"}, &wire.BlobsPage{}},
	}
	for _, m := range messages {
		if err := m.read.Decode(m.written.Append(nil)); err != nil {
			t.Errorf("%T: %v", m.written, err)
			continue
		}
		if got := reflect.ValueOf(m.read).Elem().Interface(); !reflect.DeepEqual(got, m.written) {
			t.Errorf("%T read as %+v", m.written, got)
		}
	}
}
