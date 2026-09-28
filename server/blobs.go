package server

import (
	"fmt"
	"io"
	"maps"
	"slices"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/blobs"
	"github.com/tinyshed/tinystore/server/wire"
)

// the most bytes a download's DATA carries, so that no stream holds the
// connection longer than such a frame takes to send
const transferChunk = 64 << 10

// blobsHandle is a bucket a session opened
type blobsHandle struct {
	name   string
	bucket *blobs.Bucket
}

func (s *Server) blobsMethods(methods map[wire.Method]handler) {
	methods[wire.BlobsOpen] = blobsOpen
	for _, method := range []wire.Method{
		wire.BlobsStat, wire.BlobsDelete, wire.BlobsCopy, wire.BlobsMove, wire.BlobsUsage, wire.BlobsClear,
	} {
		methods[method] = func(c *call) error { return blobsOne(c, method) }
	}
	methods[wire.BlobsScan] = blobsScan
	methods[wire.BlobsPut] = blobsPut
	methods[wire.BlobsGet] = blobsGet
}

func blobsOpen(c *call) error {
	var ask wire.BlobsBucket
	if err := ask.Decode(c.request); err != nil {
		return err
	}
	store, err := c.session.server.blobsStore(c.ctx)
	if err != nil {
		return err
	}
	var options []blobs.BucketOption
	if ask.DefaultTTL > 0 {
		options = append(options, blobs.DefaultTTL(durationOf(ask.DefaultTTL)))
	}
	if ask.MaxSize > 0 {
		options = append(options, blobs.MaxSize(int64(min(ask.MaxSize, 1<<63-1))))
	}
	bucket, err := blobs.OpenBucket(c.ctx, store, ask.Name, options...)
	if err != nil {
		return err
	}
	return respond(c, wire.Handle{Handle: c.session.blobsHandles.add(&blobsHandle{name: ask.Name, bucket: bucket})})
}

// folder is the handle's bucket under the call's owners
func (s *session) folder(ask wire.BlobsCall) (*blobs.Bucket, error) {
	handle, err := s.blobsHandles.get(ask.Handle)
	if err != nil {
		return nil, err
	}
	return handle.bucket.Of(owners(ask.Owners)...), nil
}

func blobsOne(c *call, method wire.Method) error {
	var ask wire.BlobsCall
	if err := ask.Decode(c.request); err != nil {
		return err
	}
	folder, err := c.session.folder(ask)
	if err != nil {
		return err
	}
	switch method {
	case wire.BlobsStat:
		object, found, statErr := folder.Stat(c.ctx, ask.Key)
		return answerObject(c, object, found, statErr)
	case wire.BlobsDelete:
		if err = folder.Delete(c.ctx, ask.Key, writeOptions(ask)...); err != nil {
			return err
		}
		return respond(c, wire.Empty{})
	case wire.BlobsCopy:
		object, copyErr := folder.Copy(c.ctx, ask.Key, ask.To, writeOptions(ask)...)
		return answerObject(c, object, true, copyErr)
	case wire.BlobsMove:
		object, moveErr := folder.Move(c.ctx, ask.Key, ask.To, writeOptions(ask)...)
		return answerObject(c, object, true, moveErr)
	case wire.BlobsUsage:
		usage, usageErr := folder.Usage(c.ctx)
		if usageErr != nil {
			return usageErr
		}
		return respond(c, wire.BlobsTotal{Objects: usage.Objects, Bytes: usage.Bytes})
	}
	if err = folder.Clear(c.ctx); err != nil {
		return err
	}
	return respond(c, wire.Empty{})
}

func answerObject(c *call, object blobs.Object, found bool, err error) error {
	if err != nil {
		return err
	}
	return respond(c, objectOf(object, found))
}

func objectOf(object blobs.Object, found bool) wire.BlobsObject {
	if !found {
		return wire.BlobsObject{}
	}
	return wire.BlobsObject{
		Found: true, Key: object.Key, Size: object.Size, ETag: object.ETag, ContentType: object.ContentType,
		Modified: object.Modified.UnixMilli(), Expires: unixMillis(object.Expires), Meta: object.Meta,
	}
}

func unixMillis(t time.Time) int64 {
	if t.IsZero() {
		return 0
	}
	return t.UnixMilli()
}

// writeOptions is what a call says of the object it writes, as the engine
// takes it: a method refuses an option it does not take with ErrInvalid
func writeOptions(ask wire.BlobsCall) []blobs.Option {
	var options []blobs.Option
	if ask.ContentType != nil {
		options = append(options, blobs.ContentType(*ask.ContentType))
	}
	for _, key := range slices.Sorted(maps.Keys(ask.Meta)) {
		options = append(options, blobs.Meta(key, ask.Meta[key]))
	}
	if ask.TTL > 0 {
		options = append(options, blobs.TTL(durationOf(ask.TTL)))
	}
	if ask.ExpireAt != 0 {
		options = append(options, blobs.ExpireAt(time.UnixMilli(ask.ExpireAt)))
	}
	if ask.Size >= 0 {
		options = append(options, blobs.Size(ask.Size))
	}
	if ask.IfMatch != "" {
		options = append(options, blobs.IfMatch(ask.IfMatch))
	}
	if ask.IfNoneMatch {
		options = append(options, blobs.IfNoneMatch())
	}
	return options
}

// blobsPut is an upload: the object's bytes arrive as DATA and go to the
// engine's upload as they come, and the object appears at its commit, whole,
// or not at all
func blobsPut(c *call) error {
	var ask wire.BlobsCall
	if err := ask.Decode(c.request); err != nil {
		return err
	}
	folder, err := c.session.folder(ask)
	if err != nil {
		return err
	}
	upload, err := folder.Create(c.ctx, ask.Key, writeOptions(ask)...)
	if err != nil {
		return err
	}
	defer upload.Abort()
	for last := false; !last; {
		var body []byte
		if body, last, err = c.receive(); err != nil {
			return err
		}
		_, err = upload.Write(body)
		c.consumed(body)
		if err != nil {
			return err
		}
	}
	object, err := upload.Commit(c.ctx)
	return answerObject(c, object, true, err)
}

// blobsGet is a download: the object in the RESPONSE, then its bytes as DATA,
// or the RESPONSE alone, with END, for a key that holds none. A whole read
// whose bytes do not hash to the object's ends with ERROR and corrupt
// instead of its last bytes.
func blobsGet(c *call) error {
	var ask wire.BlobsCall
	if err := ask.Decode(c.request); err != nil {
		return err
	}
	folder, err := c.session.folder(ask)
	if err != nil {
		return err
	}
	reader, found, err := folder.Open(c.ctx, ask.Key)
	if err != nil || !found {
		return answerObject(c, blobs.Object{}, false, err)
	}
	defer reader.Close()
	length, err := readRange(reader, ask)
	if err != nil {
		return err
	}
	if err = begin(c, objectOf(reader.Object, true)); err != nil {
		return err
	}
	return sendBytes(c, reader, length)
}

// readRange places a reader at the range a get asks for and says its length
func readRange(reader *blobs.Reader, ask wire.BlobsCall) (int64, error) {
	if ask.Offset > reader.Size {
		return 0, fmt.Errorf("%w: blobs: a range from byte %d of an object of %d", tinystore.ErrInvalid, ask.Offset,
			reader.Size)
	}
	length := reader.Size - ask.Offset
	if ask.Length > 0 {
		length = min(length, ask.Length)
	}
	if ask.Offset > 0 {
		if _, err := reader.Seek(ask.Offset, io.SeekStart); err != nil {
			return 0, err
		}
	}
	return length, nil
}

// sendBytes sends length bytes of r as DATA, the last with END
func sendBytes(c *call, r io.Reader, length int64) error {
	buffer := takeBody(transferChunk)
	defer giveBody(buffer)
	for {
		n := min(int64(len(buffer)), length)
		if _, err := io.ReadFull(r, buffer[:n]); err != nil {
			return err
		}
		length -= n
		if err := c.chunk(buffer[:n], length == 0); err != nil || length == 0 {
			return err
		}
	}
}

// blobsScan is a download: a page of the objects under a folder, an object a
// DATA, and a last DATA saying where the next page begins
func blobsScan(c *call) error {
	var ask wire.BlobsCall
	if err := ask.Decode(c.request); err != nil {
		return err
	}
	folder, err := c.session.folder(ask)
	if err != nil {
		return err
	}
	page, err := folder.Scan(c.ctx, blobs.Query{
		Prefix: ask.Prefix, After: ask.After,
		Limit: int(min(ask.Limit, 1<<20)),
	})
	if err != nil {
		return err
	}
	if err = begin(c, wire.Empty{}); err != nil {
		return err
	}
	for _, object := range page.Objects {
		if err = item(c, objectOf(object, true)); err != nil {
			return err
		}
	}
	return trailer(c, wire.BlobsPage{More: page.More, After: page.Next.After})
}
