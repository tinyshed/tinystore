package tinystore

import (
	"context"
	"crypto/tls"
	"fmt"
	"regexp"
	"strings"
	"time"

	"tinyshed.org/tinystore/internal/core"
	"tinyshed.org/tinystore/internal/wire"
)

// Store is the store in a directory, as this process reaches it. Close it
// once: its close stops every worker, sends what instruments changed, hands
// over the loggers' lines, and lets the directory go.
type Store struct {
	core  *core.Store
	clock *TestClock
}

// CoreHome is how this module's packages reach the store; a program has no use for it.
func (s *Store) CoreHome() core.HomeOf { return core.HomeOf{Store: s.core} }

// Option is how a store opens.
type Option func(*options)

type options struct {
	private, embedded bool
	library, binary   string
	idle              time.Duration
	idleSet           bool
	clock             time.Time
	durability        string
	encryptionKeyFile string
	selfMetrics       bool
	withoutSystem     bool
	maxSeries         int
	maxSeriesSet      bool
}

// Private starts a server of this process's own, a child that lives and dies
// with it, for tests and scripts.
func Private() Option { return func(o *options) { o.private = true } }

// Embedded runs the core inside this process, through the library Library
// names, TINYSTORE_LIBRARY, or the release's own, fetched into the user's
// cache at the first open. On Linux and macOS it needs cgo.
func Embedded() Option { return func(o *options) { o.embedded = true } }

// Library is the core's library the store in this process loads:
// tinystore_ffi.dll, libtinystore_ffi.so or .dylib.
func Library(path string) Option {
	return func(o *options) {
		o.embedded = true
		o.library = path
	}
}

// Binary is the tinystore binary a sidecar or a private child runs;
// TINYSTORE_BIN, PATH's, or the release's own, fetched into the user's cache
// at the first open, unless given.
func Binary(path string) Option { return func(o *options) { o.binary = path } }

// Idle is how long a sidecar this process starts stays once its last
// connection has gone: 30 s unless given, 0 for ever. One found running keeps its own.
func Idle(d time.Duration) Option {
	return func(o *options) {
		o.idle = d
		o.idleSet = true
	}
}

// Clock runs a private server on a test's clock, starting at this time,
// which store.Clock() moves forward instead of a test waiting.
func Clock(start time.Time) Option { return func(o *options) { o.clock = start } }

// Durability is how far a commit of the store's files goes before it
// returns: "full" is synced to the disk; "os" survives the program's crash,
// and a power loss may take the last commits. Unless given, kv, jobs, files,
// search and every database are "full", and records and metrics "os"; given,
// every file is. It is the store's, set by whoever opens it first: one found
// open with another, or with none when this asks one, is refused.
func Durability(level string) Option { return func(o *options) { o.durability = level } }

// EncryptionKeyFile is the file of the store's encryption key, 64 hex digits,
// kept apart from the data; the store reads it and never makes it.
func EncryptionKeyFile(path string) Option { return func(o *options) { o.encryptionKeyFile = path } }

// SelfMetrics measures the store into its own metrics, under names that start
// with tinystore_: its commits and their waits, its files, its memory and the
// machine it runs on, which WithoutSystem leaves out. Off unless asked for; a
// sidecar found running keeps its own.
func SelfMetrics(opts ...SelfMetricsOption) Option {
	return func(o *options) {
		o.selfMetrics = true
		for _, opt := range opts {
			opt(o)
		}
	}
}

// SelfMetricsOption is what a store's own metrics leave out.
type SelfMetricsOption func(*options)

// WithoutSystem leaves the machine out of the store's own metrics: its
// process's memory and processor, and the system's.
func WithoutSystem() SelfMetricsOption { return func(o *options) { o.withoutSystem = true } }

// MaxSeries is the metrics series the store holds at most: a new series past
// them is a limit error. 100,000 unless given; a sidecar found running keeps
// its own.
func MaxSeries(count int) Option {
	return func(o *options) {
		o.maxSeries = count
		o.maxSeriesSet = true
	}
}

// Open opens the store in a directory, and returns once it answers: through
// the directory's sidecar, found through its SERVE file or started, unless
// Private or Embedded says otherwise. A directory that cannot be served fails here.
func Open(ctx context.Context, directory string, opts ...Option) (*Store, error) {
	var o options
	for _, opt := range opts {
		opt(&o)
	}
	if err := checkDirectory(directory); err != nil {
		return nil, err
	}
	if o.durability != "" && o.durability != "full" && o.durability != "os" {
		return nil, core.Errorf(core.Invalid, "open: durability %q: it is \"full\" or \"os\"", o.durability)
	}
	if !o.clock.IsZero() && !o.private {
		return nil, core.Errorf(core.Invalid, "a clock is a private server's: a shared sidecar runs on the system's time")
	}
	if o.private && o.embedded {
		return nil, core.Errorf(core.Invalid, "a store is private or embedded, not both")
	}
	if o.maxSeriesSet && o.maxSeries < 1 {
		return nil, core.Errorf(core.Invalid, "open: MaxSeries %d: a count above 0", o.maxSeries)
	}
	started := core.Started{Durability: o.durability, EncryptionKeyFile: o.encryptionKeyFile, SelfMetrics: o.selfMetrics, WithoutSystem: o.withoutSystem, MaxSeries: uint64(max(o.maxSeries, 0))}
	if o.idleSet {
		ms, err := core.Millis(o.idle, "a sidecar's idle")
		if err != nil {
			return nil, err
		}
		started.Idle = core.Spelled(ms)
	}
	binary := func(ctx context.Context) (string, error) { return core.FindBinary(ctx, o.binary) }
	var dial func(context.Context) (*core.Connection, error)
	switch {
	case o.embedded:
		dial = core.InProcess(directory, o.library, started)
	case o.private:
		clock := ""
		if !o.clock.IsZero() {
			clock = o.clock.UTC().Format(time.RFC3339Nano)
		}
		dial = core.PrivateChild(directory, binary, clock, started)
	default:
		dial = core.Sidecar(directory, binary, started)
	}
	if o.durability != "" {
		dial = asking(dial, directory, o.durability)
	}
	return reach(ctx, core.NewLink(dial))
}

// asking dials as dial does, and refuses a store whose files commit another
// way than this program asked for: a store has one durability, its first
// opener's, and a program that needs another must not be told it has it.
func asking(dial func(context.Context) (*core.Connection, error), directory, durability string) func(context.Context) (*core.Connection, error) {
	return func(ctx context.Context) (*core.Connection, error) {
		c, err := dial(ctx)
		if err != nil {
			return nil, err
		}
		if opened := c.Session.Agreed().Durability; opened != durability {
			c.Close()
			said := fmt.Sprintf("with durability %q", opened)
			if opened == "" {
				said = "with no durability set, full but os for records and metrics"
			}
			return nil, core.Errorf(core.Invalid,
				"the store in %s is open %s, and this program asked for %q: open it without durability, or start its server with --durability %s",
				directory, said, durability, durability)
		}
		return c, nil
	}
}

// ConnectOption is how a remote server is reached.
type ConnectOption func(*connectOptions)

type connectOptions struct{ tls *tls.Config }

// TLS is the configuration a tls:// server's certificate is checked with: the
// system's roots unless given.
func TLS(config *tls.Config) ConnectOption { return func(o *connectOptions) { o.tls = config } }

// Connect connects to a remote server with a token. tls:// checks the
// server's certificate before the token leaves; tcp:// sends the token in the
// clear, for a network its operator trusts.
func Connect(ctx context.Context, url, token string, opts ...ConnectOption) (*Store, error) {
	var o connectOptions
	for _, opt := range opts {
		opt(&o)
	}
	return reach(ctx, core.NewLink(func(ctx context.Context) (*core.Connection, error) {
		return core.Dial(ctx, url, token, nil, o.tls)
	}))
}

func reach(ctx context.Context, link *core.Link) (*Store, error) {
	if _, err := link.Connection(ctx); err != nil {
		link.Close()
		return nil, err
	}
	s := &Store{core: core.NewStore(link)}
	s.clock = &TestClock{link: link}
	return s, nil
}

// two letters or more before a colon: an address, never a Windows drive
var address = regexp.MustCompile(`^([a-zA-Z][a-zA-Z0-9+.-]+):`)

// checkDirectory refuses an address where a directory belongs: Open of
// "tcp://..." would make a folder of that name and start a server in it,
// rather than reach the one meant.
func checkDirectory(directory string) error {
	scheme := address.FindStringSubmatch(directory)
	if scheme == nil {
		return nil
	}
	if s := strings.ToLower(scheme[1]); s == "tcp" || s == "tls" {
		return core.Errorf(core.Invalid, "Open takes a store's directory, and %s is a server's address: Connect reaches it", directory)
	}
	return core.Errorf(core.Invalid, "Open takes a store's directory, and %s is an address: open the directory its server serves", directory)
}

// Close stops every worker, each once its handlers under way have
// answered, sends what instruments changed, then closes the connection. A
// private child or the core in this process lets the directory go before it
// returns; the directory's sidecar goes once it has been idle.
func (s *Store) Close() error {
	s.core.Close()
	return nil
}

// Status is what a server is, as its WELCOME said it to this store's connection.
type Status struct {
	// Server is its version: v0.1.0, or (devel) for one built from a checkout.
	Server   string
	Protocol uint64
	// Engines are the engines it serves.
	Engines []string
	// Capability is admin, data or read: admin may change a schema, data
	// reads and writes, read only reads.
	Capability string
}

// Status is what the server this store reaches is: its version, protocol,
// engines and capability.
func (s *Store) Status(ctx context.Context) (Status, error) {
	c, err := s.core.Link.Connection(ctx)
	if err != nil {
		return Status{}, err
	}
	agreed, err := c.Session.Welcomed(ctx)
	if err != nil {
		return Status{}, err
	}
	return Status{agreed.Server, agreed.Protocol, agreed.Engines, agreed.Capability}, nil
}

// Clock is the test's clock of a private store opened with tinystore.Clock;
// on any other store its calls are Invalid.
func (s *Store) Clock() *TestClock { return s.clock }

// Now is the time on this machine, which a row written now is stamped with.
func (s *Store) Now() time.Time { return time.Now().UTC() }

// TestClock is the clock a private server opened with tinystore.Clock runs
// on: moving it forward expires keys, makes jobs due and ages records at once.
type TestClock struct{ link *core.Link }

// Now is the time the server's clock reads.
func (c *TestClock) Now(ctx context.Context) (time.Time, error) {
	return c.move(ctx, wire.ServerClock{})
}

// Advance moves the clock forward by d.
func (c *TestClock) Advance(ctx context.Context, d time.Duration) error {
	ms, err := core.Millis(d, "a clock's advance")
	if err != nil {
		return err
	}
	_, err = c.move(ctx, wire.ServerClock{Advance: &ms})
	return err
}

// Set sets the clock to a time, which may not be before its own.
func (c *TestClock) Set(ctx context.Context, to time.Time) error {
	at := to.UnixMilli()
	_, err := c.move(ctx, wire.ServerClock{At: &at})
	return err
}

func (c *TestClock) move(ctx context.Context, message wire.ServerClock) (time.Time, error) {
	answer, err := c.link.Call(ctx, core.Writing, wire.MethodServerClock, wire.Encode(&message))
	if err != nil {
		return time.Time{}, err
	}
	var moved wire.ServerClock
	if err := wire.Decode(answer, &moved); err != nil {
		return time.Time{}, core.Wrap(core.Protocol, err, "a clock's answer that does not read")
	}
	if moved.At == nil {
		return core.Moment(0), nil
	}
	return core.Moment(*moved.At), nil
}

// Tx is the store's transaction while its function runs: a handle takes it
// in with In, stock.In(tx), and its calls then go through it.
type Tx struct{ tx *core.StoreTx }

// CoreTx is how this module's packages reach the transaction; a program has no use for it.
func (tx *Tx) CoreTx() core.TxOf { return core.TxOf{Store: tx.tx} }

// Tx runs fn as one transaction over the handles it takes in with In: reads
// go at once; writes commit together when fn returns, after a check that
// nothing it read has changed, and fn runs again when something did, five
// times at most, so it does nothing else that must happen once. An error fn
// returns rolls it back and is Tx's.
func (s *Store) Tx(ctx context.Context, fn func(tx *Tx) error) error {
	return core.RunTx(ctx, s.core, func(tx *core.StoreTx) error { return fn(&Tx{tx}) })
}
