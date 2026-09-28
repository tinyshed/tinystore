package wire

// Protocol is the version of this protocol a HELLO and a WELCOME name.
const Protocol = 1

// the bounds a client assumes until its WELCOME states the server's
const DefaultStreamCredit = 1 << 20

// Hello is a client's first frame.
type Hello struct {
	Protocol uint64
	Client   string // a name and version, for logs
	Token    string // required on TCP
	// MaxBody is the largest body the client takes; zero takes the server's.
	MaxBody uint32
	// StreamCredit is the DATA the server may send on a stream before the
	// client grants more; zero is DefaultStreamCredit.
	StreamCredit uint32
	// Challenge is ChallengeSize random bytes from a client that found the
	// server through SERVE, which the WELCOME's Proof answers.
	Challenge []byte
}

func (h Hello) Append(dst []byte) []byte {
	m := BeginMap(dst)
	m.Uint(1, h.Protocol)
	m.Str(2, h.Client)
	if h.Token != "" {
		m.Str(3, h.Token)
	}
	if h.MaxBody != 0 {
		m.Uint(4, uint64(h.MaxBody))
	}
	if h.StreamCredit != 0 {
		m.Uint(5, uint64(h.StreamCredit))
	}
	if h.Challenge != nil {
		m.Bin(6, h.Challenge)
	}
	return m.End()
}

func (h *Hello) Decode(body []byte) error {
	d := NewDecoder(body)
	for key := range d.Fields() {
		switch key {
		case 1:
			h.Protocol = d.Uint()
		case 2:
			h.Client = d.Str()
		case 3:
			h.Token = d.Str()
		case 4:
			h.MaxBody = d.Uint32()
		case 5:
			h.StreamCredit = d.Uint32()
		case 6:
			h.Challenge = clone(d.Bin())
		}
	}
	return d.End()
}

// Welcome is the server's answer to a Hello it takes.
type Welcome struct {
	Protocol   uint64
	Server     string // its version
	Instance   []byte // sixteen random bytes a start
	Capability Capability
	// MaxBody is the largest body either side sends.
	MaxBody uint32
	// InFlight is the streams a client may have open at once.
	InFlight uint32
	// ConnectionCredit is the bytes of REQUEST and DATA bodies a client may
	// send before credit comes back, at least MaxBody.
	ConnectionCredit uint32
	// StreamCredit is the bytes of DATA a client may send on a stream before
	// credit comes back.
	StreamCredit uint32
	Engines      []string
	Now          int64 // the store's clock, unix milliseconds
	// Proof answers a local HELLO's challenge, Prove of it with the secret
	// SERVE names; nil when the HELLO carried none.
	Proof []byte
}

// Capability is what a connection may do.
type Capability string

const (
	// Admin may do everything, the schema included.
	Admin Capability = "admin"
	// Data reads and writes every engine's data and changes no schema.
	Data Capability = "data"
)

func (w Welcome) Append(dst []byte) []byte {
	m := BeginMap(dst)
	m.Uint(1, w.Protocol)
	m.Str(2, w.Server)
	m.Bin(3, w.Instance)
	m.Str(4, string(w.Capability))
	m.Uint(5, uint64(w.MaxBody))
	m.Uint(6, uint64(w.InFlight))
	m.Uint(7, uint64(w.ConnectionCredit))
	m.Uint(8, uint64(w.StreamCredit))
	m.Key(9)
	buf := AppendArray(m.Buf(), len(w.Engines))
	for _, engine := range w.Engines {
		buf = AppendStr(buf, engine)
	}
	m.SetBuf(buf)
	m.Int(10, w.Now)
	if w.Proof != nil {
		m.Bin(11, w.Proof)
	}
	return m.End()
}

func (w *Welcome) Decode(body []byte) error {
	d := NewDecoder(body)
	for key := range d.Fields() {
		switch key {
		case 1:
			w.Protocol = d.Uint()
		case 2:
			w.Server = d.Str()
		case 3:
			w.Instance = clone(d.Bin())
		case 4:
			w.Capability = Capability(d.Str())
		case 5:
			w.MaxBody = d.Uint32()
		case 6:
			w.InFlight = d.Uint32()
		case 7:
			w.ConnectionCredit = d.Uint32()
		case 8:
			w.StreamCredit = d.Uint32()
		case 9:
			w.Engines = d.Strs()
		case 10:
			w.Now = d.Int()
		case 11:
			w.Proof = clone(d.Bin())
		}
	}
	return d.End()
}

// GoAway ends a connection: the server's, or a HELLO or frame it cannot take.
type GoAway struct {
	Code    Code
	Message string
}

func (g GoAway) Append(dst []byte) []byte {
	m := BeginMap(dst)
	m.Str(1, string(g.Code))
	m.Str(2, g.Message)
	return m.End()
}

func (g *GoAway) Decode(body []byte) error {
	d := NewDecoder(body)
	for key := range d.Fields() {
		switch key {
		case 1:
			g.Code = Code(d.Str())
		case 2:
			g.Message = d.Str()
		}
	}
	return d.End()
}

// Uint32 reads an unsigned integer that fits 32 bits.
func (d *Decoder) Uint32() uint32 {
	v := d.Uint()
	if v > 1<<32-1 {
		d.fail("%d where a u32 belongs", v)
		return 0
	}
	return uint32(v)
}

// Strs reads an array of str.
func (d *Decoder) Strs() []string {
	var strs []string
	for range d.Items() {
		strs = append(strs, d.Str())
	}
	return strs
}

// clone keeps bytes that would otherwise alias a body the reader reuses
func clone(b []byte) []byte {
	if b == nil {
		return nil
	}
	return append(make([]byte, 0, len(b)), b...)
}
