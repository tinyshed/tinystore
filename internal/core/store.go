package core

import (
	"context"
	"fmt"
	"reflect"
	"regexp"
	"strconv"
	"sync"
	"time"
)

// The stages a store's close stops what runs in them, in order: workers
// finish their handlers, instruments send what they changed, loggers hand
// over their lines, and what follows a stream is cancelled before the
// connection ends.
const (
	StopWorkers = iota
	StopInstruments
	StopLoggers
	StopFollowers
	stopStages
)

// Store is what every package of this SDK shares of a store: the link to it,
// what its close stops, and what a package keeps once a store.
type Store struct {
	Link    *Link
	mu      sync.Mutex
	stops   [stopStages]map[int]func()
	nextID  int
	closed  bool
	kept    map[string]any
	closing chan struct{}
}

// NewStore is a store reached through link.
func NewStore(link *Link) *Store {
	s := &Store{Link: link, kept: map[string]any{}, closing: make(chan struct{})}
	for i := range s.stops {
		s.stops[i] = map[int]func(){}
	}
	return s
}

// OnClose has the store's close call stop at its stage; the function it
// gives takes stop off once what it stops has ended by itself.
func (s *Store) OnClose(stage int, stop func()) (forget func()) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closed {
		go stop()
		return func() {}
	}
	s.nextID++
	id := s.nextID
	s.stops[stage][id] = stop
	return func() {
		s.mu.Lock()
		delete(s.stops[stage], id)
		s.mu.Unlock()
	}
}

// Closing is closed once the store's close begins.
func (s *Store) Closing() <-chan struct{} { return s.closing }

// Kept is what a package keeps once a store under key, made the first time.
func (s *Store) Kept(key string, make func() any) any {
	s.mu.Lock()
	defer s.mu.Unlock()
	found, ok := s.kept[key]
	if !ok {
		found = make()
		s.kept[key] = found
	}
	return found
}

// Close stops what runs, a stage at a time, then closes the link.
func (s *Store) Close() {
	s.mu.Lock()
	if s.closed {
		s.mu.Unlock()
		return
	}
	s.closed = true
	close(s.closing)
	stages := s.stops
	s.mu.Unlock()
	for _, stops := range stages {
		var running sync.WaitGroup
		for _, stop := range stops {
			running.Add(1)
			go func() {
				defer running.Done()
				stop()
			}()
		}
		running.Wait()
	}
	s.Link.Close()
}

// Home is where a handle opens: the store, or a database whose file keeps it.
// Only this module's types satisfy it.
type Home interface {
	CoreHome() HomeOf
}

// HomeOf is a handle's home as the packages read it.
type HomeOf struct {
	Store *Store
	// Database is the database whose file keeps the handle; nil for the store's own files.
	Database Database
}

// Database is a database a handle is kept in: its handle on a connection, its
// name as errors say it, and how its handles reach the store.
type Database interface {
	Describe() string
	Handle(ctx context.Context, c *Connection) (uint64, error)
	// Keep is the opener of a handle kept in the database's file: the one made
	// first for the same open, so that the database's transaction opens each
	// once, before it holds the writer.
	Keep(method uint16, body []byte, make func(ctx context.Context, c *Connection) ([]byte, error)) *Opener
	// Via is how a handle kept in the database's file reaches the store
	// outside a transaction; inside the database's own it is refused, since it
	// would wait for the writer the transaction holds.
	Via(what string) Via
}

// InTx is a transaction a handle is taken into: the store's or a database's.
type InTx interface {
	CoreTx() TxOf
}

// TxOf is a transaction as the packages read it: one of the two.
type TxOf struct {
	Store    *StoreTx
	Database DatabaseTx
}

// DatabaseTx is a database's transaction as a handle taken into it sees it:
// its database, how calls go on its stream, and a call of another engine in it.
type DatabaseTx interface {
	Database() Database
	Via() Via
	Nested(ctx context.Context, method uint16, body []byte) ([]byte, error)
}

var name = regexp.MustCompile(`^[a-z0-9][a-z0-9_-]{0,63}$`)

// CheckName refuses a name the store would, before anything leaves.
func CheckName(n, of string) error {
	if !name.MatchString(n) {
		return Errorf(Invalid, "the %s name %q: a name is [a-z0-9][a-z0-9_-]{0,63}", of, n)
	}
	return nil
}

// KeyText is a key or an owner as the store spells it: text as it is, an
// integer its decimal text.
func KeyText(key any) (string, error) {
	switch k := key.(type) {
	case string:
		return k, nil
	case []byte:
		return string(k), nil
	case fmt.Stringer:
		return k.String(), nil
	}
	v := reflect.ValueOf(key)
	switch v.Kind() {
	case reflect.Int, reflect.Int8, reflect.Int16, reflect.Int32, reflect.Int64:
		return strconv.FormatInt(v.Int(), 10), nil
	case reflect.Uint, reflect.Uint8, reflect.Uint16, reflect.Uint32, reflect.Uint64:
		return strconv.FormatUint(v.Uint(), 10), nil
	case reflect.String:
		return v.String(), nil
	}
	return "", Errorf(Invalid, "the key %v: a key is text or an integer", key)
}

// Millis is a span's milliseconds; a negative one is refused.
func Millis(d time.Duration, of string) (uint64, error) {
	if d < 0 {
		return 0, Errorf(Invalid, "%s of %s, before nothing", of, d)
	}
	return uint64(d / time.Millisecond), nil
}

// UnixMs is a time's unix milliseconds.
func UnixMs(t time.Time) int64 { return t.UnixMilli() }

// Moment is a time the server named in unix milliseconds.
func Moment(ms int64) time.Time { return time.UnixMilli(ms).UTC() }

// MomentOf is a time the server named, the zero time for none or 0.
func MomentOf(ms *int64) time.Time {
	if ms == nil || *ms == 0 {
		return time.Time{}
	}
	return Moment(*ms)
}

// Spelled is milliseconds as a span is written, largest units first: 1h30m, 0s.
func Spelled(ms uint64) string {
	if ms == 0 {
		return "0s"
	}
	text := ""
	for _, unit := range []struct {
		name string
		size uint64
	}{{"w", 604_800_000}, {"d", 86_400_000}, {"h", 3_600_000}, {"m", 60_000}, {"s", 1000}, {"ms", 1}} {
		if ms >= unit.size {
			text += strconv.FormatUint(ms/unit.size, 10) + unit.name
			ms %= unit.size
		}
	}
	return text
}
