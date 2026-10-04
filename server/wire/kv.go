package wire

import (
	"strconv"
	"unicode/utf8"
)

// The kv engine's methods, docs/wire.md#kv.
const (
	KVOpen   Method = 0x0101
	KVGet    Method = 0x0102
	KVHas    Method = 0x0103
	KVSet    Method = 0x0104
	KVDelete Method = 0x0105
	KVTake   Method = 0x0106
	KVTouch  Method = 0x0107
	KVAdd    Method = 0x0108
	KVMax    Method = 0x0109
	KVClear  Method = 0x010a
	KVBatch  Method = 0x010b
	KVView   Method = 0x010c
	KVScan   Method = 0x010d

	KVAllow     Method = 0x010e
	KVConfigure Method = 0x010f
	KVWatch     Method = 0x0110
	KVRun       Method = 0x0111
	KVUsage     Method = 0x0112
	KVRefund    Method = 0x0113
)

// KVBucket is kv.open's request: a bucket of values, counters, a config, a
// limiter, once's answers or a quota, by name, with its handle's options. A
// duration is milliseconds, zero for none; a limiter is a bucket with a rate,
// and a quota one with windows. In names the SQL database whose file keeps the
// bucket, which sql.open opened first: a batch of it writes the bucket's keys.
type KVBucket struct {
	Name       string
	Counters   bool
	DefaultTTL int64
	Sliding    int64
	LoseAtMost int64
	Config     bool
	Rate       uint64 // a limiter's requests every Per
	Per        int64
	Burst      uint64
	Once       bool // the answers kv.run keeps
	Windows    []KVWindow
	In         string
}

// KVWindow is one of a quota's windows: a key may use up to Limit every Per
// milliseconds, from its first use.
type KVWindow struct {
	Name  string
	Limit uint64
	Per   int64
}

func (b KVBucket) Append(dst []byte) []byte {
	m := BeginMap(dst)
	m.Str(1, b.Name)
	if b.Counters {
		m.Bool(2, true)
	}
	if b.DefaultTTL != 0 {
		m.Int(3, b.DefaultTTL)
	}
	if b.Sliding != 0 {
		m.Int(4, b.Sliding)
	}
	if b.LoseAtMost != 0 {
		m.Int(5, b.LoseAtMost)
	}
	if b.Config {
		m.Bool(6, true)
	}
	if b.Rate != 0 {
		m.Uint(7, b.Rate)
	}
	if b.Per != 0 {
		m.Int(8, b.Per)
	}
	if b.Burst != 0 {
		m.Uint(9, b.Burst)
	}
	if b.Once {
		m.Bool(10, true)
	}
	if len(b.Windows) > 0 {
		m.Key(11)
		buf := AppendArray(m.Buf(), len(b.Windows))
		for _, w := range b.Windows {
			fields := BeginMap(buf)
			optionalStr(&fields, 1, w.Name)
			optionalUint(&fields, 2, w.Limit)
			optionalInt(&fields, 3, w.Per)
			buf = fields.End()
		}
		m.SetBuf(buf)
	}
	optionalStr(&m, 12, b.In)
	return m.End()
}

func (b *KVBucket) Decode(body []byte) error {
	d := NewDecoder(body)
	for key := range d.Fields() {
		switch key {
		case 1:
			b.Name = d.Str()
		case 2:
			b.Counters = d.Bool()
		case 3:
			b.DefaultTTL = d.Duration()
		case 4:
			b.Sliding = d.Duration()
		case 5:
			b.LoseAtMost = d.Duration()
		case 6:
			b.Config = d.Bool()
		case 7:
			b.Rate = d.Uint()
		case 8:
			b.Per = d.Duration()
		case 9:
			b.Burst = d.Uint()
		case 10:
			b.Once = d.Bool()
		case 11:
			for range d.Items() {
				b.Windows = append(b.Windows, d.kvWindow())
			}
		case 12:
			b.In = d.Str()
		}
	}
	return d.End()
}

func (d *Decoder) kvWindow() KVWindow {
	var w KVWindow
	for field := range d.Fields() {
		switch field {
		case 1:
			w.Name = d.Str()
		case 2:
			w.Limit = d.Uint()
		case 3:
			w.Per = d.Duration()
		}
	}
	return w
}

// Handle answers an open with the number the calls on its handle carry.
type Handle struct {
	Handle uint64
}

func (h Handle) Append(dst []byte) []byte {
	m := BeginMap(dst)
	m.Uint(1, h.Handle)
	return m.End()
}

func (h *Handle) Decode(body []byte) error {
	d := NewDecoder(body)
	for key := range d.Fields() {
		if key == 1 {
			h.Handle = d.Uint()
		}
	}
	return d.End()
}

// KVCall is the request of every kv method but open, batch and view: the key
// it is about, or the branch a clear or a scan is, and what the method takes.
// A key is its text: a str, a bin, or an integer by its decimal spelling.
type KVCall struct {
	Handle    uint64
	Owners    []string
	Key       string
	Value     KVValue
	TTL       int64 // milliseconds
	ExpireAt  int64 // unix milliseconds
	IfVersion []byte
	IfAbsent  bool
	N         int64  // what add adds, and max compares
	After     string // where a scan's page begins
	Limit     uint64 // the keys a scan's page returns
}

func (c KVCall) Append(dst []byte) []byte {
	m := BeginMap(dst)
	c.appendFields(&m)
	return m.End()
}

func (c KVCall) appendFields(m *Map) {
	m.Uint(1, c.Handle)
	if len(c.Owners) > 0 {
		m.Key(2)
		buf := AppendArray(m.Buf(), len(c.Owners))
		for _, owner := range c.Owners {
			buf = AppendKey(buf, owner)
		}
		m.SetBuf(buf)
	}
	if c.Key != "" {
		m.Key(3)
		m.SetBuf(AppendKey(m.Buf(), c.Key))
	}
	if c.Value.Kind != KVNothing {
		m.Key(4)
		m.SetBuf(c.Value.Append(m.Buf()))
	}
	c.appendOptions(m)
}

func (c KVCall) appendOptions(m *Map) {
	if c.TTL != 0 {
		m.Int(5, c.TTL)
	}
	if c.ExpireAt != 0 {
		m.Int(6, c.ExpireAt)
	}
	if c.IfVersion != nil {
		m.Bin(7, c.IfVersion)
	}
	if c.IfAbsent {
		m.Bool(8, true)
	}
	if c.N != 0 {
		m.Int(9, c.N)
	}
	if c.After != "" {
		m.Key(10)
		m.SetBuf(AppendKey(m.Buf(), c.After))
	}
	if c.Limit != 0 {
		m.Uint(11, c.Limit)
	}
}

func (c *KVCall) Decode(body []byte) error {
	d := NewDecoder(body)
	for key := range d.Fields() {
		c.decodeField(&d, key)
	}
	return d.End()
}

func (c *KVCall) decodeField(d *Decoder, key uint64) {
	switch key {
	case 1:
		c.Handle = d.Uint()
	case 2:
		for range d.Items() {
			c.Owners = append(c.Owners, d.KeyText())
		}
	case 3:
		c.Key = d.KeyText()
	case 4:
		c.Value = d.KVValue()
	case 5:
		c.TTL = d.Duration()
	case 6:
		c.ExpireAt = d.Int()
	case 7:
		c.IfVersion = clone(d.Bin())
	case 8:
		c.IfAbsent = d.Bool()
	case 9:
		c.N = d.Int()
	case 10:
		c.After = d.KeyText()
	case 11:
		c.Limit = d.Uint()
	}
}

// KVValue is a value as its row keeps it: nothing, an integer or bytes.
type KVValue struct {
	Kind  KVKind
	Int   int64
	Bytes []byte
}

// KVKind is which of the three a KVValue holds.
type KVKind uint8

const (
	KVNothing KVKind = iota
	KVInt
	KVBytes
)

// Append writes nil, an integer or a bin.
func (v KVValue) Append(dst []byte) []byte {
	switch v.Kind {
	case KVInt:
		return AppendInt(dst, v.Int)
	case KVBytes:
		return AppendBin(dst, v.Bytes)
	}
	return AppendNil(dst)
}

// KVValue reads nil, an integer or a bin, the bin aliasing the body.
func (d *Decoder) KVValue() KVValue {
	switch d.Type() {
	case TypeNil:
		d.Nil()
		return KVValue{}
	case TypeInt:
		return KVValue{Kind: KVInt, Int: d.Int()}
	case TypeBin:
		return KVValue{Kind: KVBytes, Bytes: d.Bin()}
	}
	d.fail("%s where a kv value, nil, an integer or a bin, belongs", d.Type())
	return KVValue{}
}

// KeyText reads a key's text: a str or a bin as it is, an integer as its
// decimal spelling, so that 42 and "42" name one key.
func (d *Decoder) KeyText() string {
	switch d.Type() {
	case TypeStr:
		return string(d.StrBytes())
	case TypeBin:
		return string(d.Bin())
	case TypeInt:
		probe := *d
		if n := probe.Int(); probe.err == nil {
			*d = probe
			return strconv.FormatInt(n, 10)
		}
		return strconv.FormatUint(d.Uint(), 10)
	}
	d.fail("%s where a key, a str, a bin or an integer, belongs", d.Type())
	return ""
}

// AppendKey writes a key's text as a str, or as a bin when it is not UTF-8.
func AppendKey(dst []byte, text string) []byte {
	if utf8.ValidString(text) {
		return AppendStr(dst, text)
	}
	return AppendBin(dst, []byte(text))
}

// Duration reads milliseconds, which are never negative.
func (d *Decoder) Duration() int64 {
	ms := d.Int()
	if ms < 0 {
		d.fail("a duration of %d milliseconds", ms)
		return 0
	}
	return ms
}

// KVEntry answers a call about one key, and is an item of a scan.
type KVEntry struct {
	// Found says a live key held a value, or for set that it wrote. It is
	// false when IfAbsent found a live key, whose entry this is.
	Found   bool
	Value   KVValue
	Version []byte
	// Expires is unix milliseconds, zero for a key that never expires.
	Expires int64
	// Key is a scan item's alone.
	Key string
}

func (e KVEntry) Append(dst []byte) []byte {
	m := BeginMap(dst)
	if e.Found {
		m.Bool(1, true)
	}
	if e.Value.Kind != KVNothing {
		m.Key(2)
		m.SetBuf(e.Value.Append(m.Buf()))
	}
	if e.Version != nil {
		m.Bin(3, e.Version)
	}
	if e.Expires != 0 {
		m.Int(4, e.Expires)
	}
	if e.Key != "" {
		m.Key(5)
		m.SetBuf(AppendKey(m.Buf(), e.Key))
	}
	return m.End()
}

func (e *KVEntry) Decode(body []byte) error {
	d := NewDecoder(body)
	e.decode(&d)
	return d.End()
}

func (e *KVEntry) decode(d *Decoder) {
	for key := range d.Fields() {
		switch key {
		case 1:
			e.Found = d.Bool()
		case 2:
			e.Value = d.KVValue()
			e.Value.Bytes = clone(e.Value.Bytes)
		case 3:
			e.Version = clone(d.Bin())
		case 4:
			e.Expires = d.Int()
		case 5:
			e.Key = d.KeyText()
		}
	}
}

// KVPage ends a scan: More says its limit, or its bytes, ended the page
// before the branch did, and After is the key the next page begins after.
type KVPage struct {
	More  bool
	After string
}

func (p KVPage) Append(dst []byte) []byte {
	m := BeginMap(dst)
	if p.More {
		m.Bool(1, true)
	}
	if p.After != "" {
		m.Key(2)
		m.SetBuf(AppendKey(m.Buf(), p.After))
	}
	return m.End()
}

func (p *KVPage) Decode(body []byte) error {
	d := NewDecoder(body)
	for key := range d.Fields() {
		switch key {
		case 1:
			p.More = d.Bool()
		case 2:
			p.After = d.KeyText()
		}
	}
	return d.End()
}

// KVCalls is kv.batch's request, calls run in one transaction that a failure of
// any rolls back, and kv.view's, reads from one snapshot. Each is a KVCall's
// fields with its method under key 0.
type KVCalls struct {
	Calls []KVOperation
}

type KVOperation struct {
	Method Method
	KVCall
}

func (b KVCalls) Append(dst []byte) []byte {
	m := BeginMap(dst)
	m.Key(1)
	m.SetBuf(appendOperations(m.Buf(), b.Calls))
	return m.End()
}

func (b *KVCalls) Decode(body []byte) error {
	d := NewDecoder(body)
	for key := range d.Fields() {
		if key == 1 {
			b.Calls = d.kvOperations()
		}
	}
	return d.End()
}

// appendOperations writes calls, each a KVCall's fields with its method under
// key 0
func appendOperations(dst []byte, calls []KVOperation) []byte {
	buf := AppendArray(dst, len(calls))
	for _, call := range calls {
		op := BeginMap(buf)
		op.Uint(0, uint64(call.Method))
		call.appendFields(&op)
		buf = op.End()
	}
	return buf
}

func (d *Decoder) kvOperations() []KVOperation {
	var calls []KVOperation
	for range d.Items() {
		var op KVOperation
		for field := range d.Fields() {
			if field == 0 {
				op.Method = Method(d.Uint16())
				continue
			}
			op.decodeField(d, field)
		}
		calls = append(calls, op)
	}
	return calls
}

// KVChanges are keys of one bucket a SQL batch writes after its statements
// and its jobs, in the same transaction: the bucket opened with kv.open's in,
// the batch's database, and each call a kv.set, kv.delete or kv.clear.
type KVChanges struct {
	Handle uint64
	Calls  []KVOperation
}

func (c KVChanges) Append(dst []byte) []byte {
	m := BeginMap(dst)
	m.Uint(1, c.Handle)
	m.Key(2)
	m.SetBuf(appendOperations(m.Buf(), c.Calls))
	return m.End()
}

func (c *KVChanges) Decode(body []byte) error {
	d := NewDecoder(body)
	c.decode(&d)
	return d.End()
}

func (c *KVChanges) decode(d *Decoder) {
	for key := range d.Fields() {
		switch key {
		case 1:
			c.Handle = d.Uint()
		case 2:
			c.Calls = d.kvOperations()
		}
	}
}

// KVResults answers a batch or a view: an entry a call, in their order.
type KVResults struct {
	Entries []KVEntry
}

func (r KVResults) Append(dst []byte) []byte {
	m := BeginMap(dst)
	m.Key(1)
	buf := AppendArray(m.Buf(), len(r.Entries))
	for _, entry := range r.Entries {
		buf = entry.Append(buf)
	}
	m.SetBuf(buf)
	return m.End()
}

func (r *KVResults) Decode(body []byte) error {
	d := NewDecoder(body)
	for key := range d.Fields() {
		if key != 1 {
			continue
		}
		for range d.Items() {
			var entry KVEntry
			entry.decode(&d)
			r.Entries = append(r.Entries, entry)
		}
	}
	return d.End()
}

// Uint16 reads an unsigned integer that fits 16 bits, a method's.
func (d *Decoder) Uint16() uint16 {
	v := d.Uint()
	if v > 1<<16-1 {
		d.fail("%d where a u16 belongs", v)
		return 0
	}
	return uint16(v)
}

// Empty is a message without fields: an answer that says only that its call
// was done.
type Empty struct{}

func (Empty) Append(dst []byte) []byte {
	return AppendMap(dst, 0)
}

func (*Empty) Decode(body []byte) error {
	d := NewDecoder(body)
	for range d.Fields() {
	}
	return d.End()
}

// KVAllowance is kv.allow's answer, and a quota's kv.usage: whether the
// requests pass, how many more would pass now, and, when they do not, how
// many milliseconds until they would, rounded up; and a quota's windows.
type KVAllowance struct {
	OK         bool
	Left       uint64
	RetryAfter uint64
	Windows    []KVWindowUsage
}

// KVWindowUsage is one of a quota's windows of a key: what it used of its
// limit, what is left, and when it resets, in unix milliseconds, zero for a
// window not started.
type KVWindowUsage struct {
	Name    string
	Used    uint64
	Limit   uint64
	Left    uint64
	ResetAt int64
}

func (a KVAllowance) Append(dst []byte) []byte {
	m := BeginMap(dst)
	if a.OK {
		m.Bool(1, true)
	}
	if a.Left != 0 {
		m.Uint(2, a.Left)
	}
	if a.RetryAfter != 0 {
		m.Uint(3, a.RetryAfter)
	}
	if len(a.Windows) > 0 {
		m.Key(4)
		buf := AppendArray(m.Buf(), len(a.Windows))
		for _, w := range a.Windows {
			fields := BeginMap(buf)
			optionalStr(&fields, 1, w.Name)
			optionalUint(&fields, 2, w.Used)
			optionalUint(&fields, 3, w.Limit)
			optionalUint(&fields, 4, w.Left)
			optionalInt(&fields, 5, w.ResetAt)
			buf = fields.End()
		}
		m.SetBuf(buf)
	}
	return m.End()
}

func (a *KVAllowance) Decode(body []byte) error {
	d := NewDecoder(body)
	for key := range d.Fields() {
		switch key {
		case 1:
			a.OK = d.Bool()
		case 2:
			a.Left = d.Uint()
		case 3:
			a.RetryAfter = d.Uint()
		case 4:
			for range d.Items() {
				a.Windows = append(a.Windows, d.kvWindowUsage())
			}
		}
	}
	return d.End()
}

func (d *Decoder) kvWindowUsage() KVWindowUsage {
	var w KVWindowUsage
	for field := range d.Fields() {
		switch field {
		case 1:
			w.Name = d.Str()
		case 2:
			w.Used = d.Uint()
		case 3:
			w.Limit = d.Uint()
		case 4:
			w.Left = d.Uint()
		case 5:
			w.ResetAt = d.Int()
		}
	}
	return w
}

// KVConfigChange is kv.configure's request: the fields a config keeps, a path and
// its JSON each, and the paths it forgets, in one transaction.
type KVConfigChange struct {
	Handle uint64
	Set    []string // path, JSON, path, JSON…
	Reset  []string
}

func (c KVConfigChange) Append(dst []byte) []byte {
	m := BeginMap(dst)
	m.Uint(1, c.Handle)
	if len(c.Set) > 0 {
		m.Key(2)
		m.SetBuf(appendStrs(m.Buf(), c.Set))
	}
	if len(c.Reset) > 0 {
		m.Key(3)
		m.SetBuf(appendStrs(m.Buf(), c.Reset))
	}
	return m.End()
}

func (c *KVConfigChange) Decode(body []byte) error {
	d := NewDecoder(body)
	for key := range d.Fields() {
		switch key {
		case 1:
			c.Handle = d.Uint()
		case 2:
			c.Set = d.Strs()
		case 3:
			c.Reset = d.Strs()
		}
	}
	if len(c.Set)%2 != 0 {
		d.Fail("set holds %d strings, not a path and its JSON each", len(c.Set))
	}
	return d.End()
}

// KVKept is an item of kv.watch: a config's kept fields, a path and its JSON
// each, and how many changes the server's process has made to them.
type KVKept struct {
	Changes uint64
	Fields  []string
}

func (k KVKept) Append(dst []byte) []byte {
	m := BeginMap(dst)
	if k.Changes != 0 {
		m.Uint(1, k.Changes)
	}
	if len(k.Fields) > 0 {
		m.Key(2)
		m.SetBuf(appendStrs(m.Buf(), k.Fields))
	}
	return m.End()
}

func (k *KVKept) Decode(body []byte) error {
	d := NewDecoder(body)
	for key := range d.Fields() {
		switch key {
		case 1:
			k.Changes = d.Uint()
		case 2:
			k.Fields = d.Strs()
		}
	}
	if len(k.Fields)%2 != 0 {
		d.Fail("fields holds %d strings, not a path and its JSON each", len(k.Fields))
	}
	return d.End()
}
