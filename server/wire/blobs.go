package wire

// The blobs engine's methods, docs/wire.md#blobs.
const (
	BlobsOpen   Method = 0x0301
	BlobsStat   Method = 0x0302
	BlobsDelete Method = 0x0303
	BlobsCopy   Method = 0x0304
	BlobsMove   Method = 0x0305
	BlobsUsage  Method = 0x0306
	BlobsClear  Method = 0x0307
	BlobsScan   Method = 0x0308
	BlobsPut    Method = 0x0309
	BlobsGet    Method = 0x030a
)

// BlobsBucket is blobs.open's request: a bucket by name, with its handle's
// options. DefaultTTL is milliseconds, zero for none; MaxSize bounds its
// objects' bytes, zero for no bound.
type BlobsBucket struct {
	Name       string
	DefaultTTL int64
	MaxSize    uint64
}

func (b BlobsBucket) Append(dst []byte) []byte {
	m := BeginMap(dst)
	m.Str(1, b.Name)
	optionalInt(&m, 2, b.DefaultTTL)
	optionalUint(&m, 3, b.MaxSize)
	return m.End()
}

func (b *BlobsBucket) Decode(body []byte) error {
	d := NewDecoder(body)
	for key := range d.Fields() {
		switch key {
		case 1:
			b.Name = d.Str()
		case 2:
			b.DefaultTTL = d.Duration()
		case 3:
			b.MaxSize = d.Uint()
		}
	}
	return d.End()
}

// BlobsCall is the request of every blobs method but open: an object by its
// key under the folder its owners name, or that folder for usage, clear and
// scan, and what the method takes. A key is a path; an owner is a segment, a
// str or an integer.
type BlobsCall struct {
	Handle      uint64
	Owners      []string
	Key         string
	To          string            // copy's and move's destination
	ContentType *string           // nil keeps what the object has, or is none
	Meta        map[string]string // nil keeps what the object has; otherwise it replaces all
	TTL         int64             // milliseconds
	ExpireAt    int64             // unix milliseconds
	Size        int64             // a put's length; -1, and absent, for unknown
	IfMatch     string            // as an HTTP If-Match header spells it
	IfNoneMatch bool
	Prefix      string // a scan's
	After       string // a scan's
	Limit       uint64 // a scan's
	Offset      int64  // a get's first byte
	Length      int64  // a get's bytes; zero reads to the end
}

func (c BlobsCall) Append(dst []byte) []byte {
	m := BeginMap(dst)
	m.Uint(1, c.Handle)
	if len(c.Owners) > 0 {
		m.Key(2)
		m.SetBuf(appendStrs(m.Buf(), c.Owners))
	}
	optionalStr(&m, 3, c.Key)
	optionalStr(&m, 4, c.To)
	if c.ContentType != nil {
		m.Str(5, *c.ContentType)
	}
	if c.Meta != nil {
		m.Key(6)
		m.SetBuf(appendNames(m.Buf(), c.Meta))
	}
	optionalInt(&m, 7, c.TTL)
	optionalInt(&m, 8, c.ExpireAt)
	if c.Size >= 0 {
		m.Int(9, c.Size)
	}
	c.appendTail(&m)
	return m.End()
}

func (c BlobsCall) appendTail(m *Map) {
	optionalStr(m, 10, c.IfMatch)
	if c.IfNoneMatch {
		m.Bool(11, true)
	}
	optionalStr(m, 12, c.Prefix)
	optionalStr(m, 13, c.After)
	optionalUint(m, 14, c.Limit)
	optionalInt(m, 15, c.Offset)
	optionalInt(m, 16, c.Length)
}

func (c *BlobsCall) Decode(body []byte) error {
	d := NewDecoder(body)
	c.Size = -1
	for key := range d.Fields() {
		if key <= 9 {
			c.decodeHead(&d, key)
			continue
		}
		c.decodeTail(&d, key)
	}
	return d.End()
}

func (c *BlobsCall) decodeHead(d *Decoder, key uint64) {
	switch key {
	case 1:
		c.Handle = d.Uint()
	case 2:
		for range d.Items() {
			c.Owners = append(c.Owners, d.KeyText())
		}
	case 3:
		c.Key = d.Str()
	case 4:
		c.To = d.Str()
	case 5:
		typed := d.Str()
		c.ContentType = &typed
	case 6:
		c.Meta = d.nameMap()
	case 7:
		c.TTL = d.Duration()
	case 8:
		c.ExpireAt = d.Int()
	case 9:
		c.Size = d.Duration()
	}
}

func (c *BlobsCall) decodeTail(d *Decoder, key uint64) {
	switch key {
	case 10:
		c.IfMatch = d.Str()
	case 11:
		c.IfNoneMatch = d.Bool()
	case 12:
		c.Prefix = d.Str()
	case 13:
		c.After = d.Str()
	case 14:
		c.Limit = d.Uint()
	case 15:
		c.Offset = d.Duration()
	case 16:
		c.Length = d.Duration()
	}
}

func appendStrs(dst []byte, strs []string) []byte {
	dst = AppendArray(dst, len(strs))
	for _, s := range strs {
		dst = AppendStr(dst, s)
	}
	return dst
}

// BlobsObject is an object as the file knows it: stat's, copy's, move's and
// put's answer, get's header, and a scan's item. Found is false for a key
// that holds no live object, and then nothing else is set.
type BlobsObject struct {
	Found       bool
	Key         string
	Size        int64
	ETag        string
	ContentType string
	Modified    int64 // unix milliseconds
	Expires     int64 // unix milliseconds; zero for an object that does not expire
	Meta        map[string]string
}

func (o BlobsObject) Append(dst []byte) []byte {
	m := BeginMap(dst)
	if !o.Found {
		return m.End()
	}
	m.Bool(1, true)
	m.Str(2, o.Key)
	m.Int(3, o.Size)
	m.Str(4, o.ETag)
	optionalStr(&m, 5, o.ContentType)
	m.Int(6, o.Modified)
	optionalInt(&m, 7, o.Expires)
	if len(o.Meta) > 0 {
		m.Key(8)
		m.SetBuf(appendNames(m.Buf(), o.Meta))
	}
	return m.End()
}

func (o *BlobsObject) Decode(body []byte) error {
	d := NewDecoder(body)
	for key := range d.Fields() {
		switch key {
		case 1:
			o.Found = d.Bool()
		case 2:
			o.Key = d.Str()
		case 3:
			o.Size = d.Duration()
		case 4:
			o.ETag = d.Str()
		case 5:
			o.ContentType = d.Str()
		case 6:
			o.Modified = d.Int()
		case 7:
			o.Expires = d.Int()
		case 8:
			o.Meta = d.nameMap()
		}
	}
	return d.End()
}

// BlobsTotal is usage's answer: the objects under a folder and their bytes.
type BlobsTotal struct {
	Objects int64
	Bytes   int64
}

func (u BlobsTotal) Append(dst []byte) []byte {
	m := BeginMap(dst)
	m.Int(1, u.Objects)
	m.Int(2, u.Bytes)
	return m.End()
}

func (u *BlobsTotal) Decode(body []byte) error {
	d := NewDecoder(body)
	for key := range d.Fields() {
		switch key {
		case 1:
			u.Objects = d.Int()
		case 2:
			u.Bytes = d.Int()
		}
	}
	return d.End()
}

// BlobsPage ends a scan: More says the limit ended it before the folder did,
// and After is the key the next page begins after.
type BlobsPage struct {
	More  bool
	After string
}

func (p BlobsPage) Append(dst []byte) []byte {
	m := BeginMap(dst)
	if p.More {
		m.Bool(1, true)
	}
	optionalStr(&m, 2, p.After)
	return m.End()
}

func (p *BlobsPage) Decode(body []byte) error {
	d := NewDecoder(body)
	for key := range d.Fields() {
		switch key {
		case 1:
			p.More = d.Bool()
		case 2:
			p.After = d.Str()
		}
	}
	return d.End()
}
