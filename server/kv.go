package server

import (
	"context"
	"encoding/json"
	"fmt"
	"maps"
	"math"
	"slices"
	"strconv"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/kv"
	"github.com/tinyshed/tinystore/server/wire"
)

// kvHandle is a bucket of values, counters, a config or a limiter a session
// opened: one of the four is set
type kvHandle struct {
	name     string
	values   *kv.Bucket[kv.Raw]
	counters *kv.Counters
	config   *kv.RawConfig
	limiter  *kv.Limiter
	state    *kv.Store
}

func (s *Server) kvMethods(methods map[wire.Method]handler) {
	methods[wire.KVOpen] = kvOpen
	for _, method := range []wire.Method{
		wire.KVGet, wire.KVHas, wire.KVSet, wire.KVDelete, wire.KVTake, wire.KVTouch, wire.KVAdd, wire.KVMax,
		wire.KVClear,
	} {
		methods[method] = func(c *call) error { return kvOne(c, method) }
	}
	methods[wire.KVBatch] = kvBatch
	methods[wire.KVView] = kvView
	methods[wire.KVScan] = kvScan
	methods[wire.KVAllow] = kvAllow
	methods[wire.KVConfigure] = kvConfigure
	methods[wire.KVWatch] = kvWatch
}

func kvOpen(c *call) error {
	var ask wire.KVBucket
	if err := ask.Decode(c.request); err != nil {
		return err
	}
	state, err := c.session.server.kvStore(c.ctx)
	if err != nil {
		return err
	}
	opened := &kvHandle{name: ask.Name, state: state}
	switch {
	case kinds(ask) > 1:
		err = fmt.Errorf("%w: kv: %q opened as more than one of counters, a config and a limiter",
			tinystore.ErrInvalid, ask.Name)
	case ask.Counters:
		opened.counters, err = kv.OpenCounters(c.ctx, state, ask.Name, counterOptions(ask)...)
	case ask.Config:
		opened.config, err = kv.OpenRawConfig(c.ctx, state, ask.Name)
	case ask.Rate > 0:
		opened.limiter, err = kv.OpenLimiter(c.ctx, state, ask.Name, limiterOptions(ask)...)
	default:
		opened.values, err = kv.OpenBucket[kv.Raw](c.ctx, state, ask.Name, bucketOptions(ask)...)
	}
	if err != nil {
		return err
	}
	return respond(c, wire.Handle{Handle: c.session.kvHandles.add(opened)})
}

// kinds is how many of counters, a config and a limiter a bucket asks to be
func kinds(ask wire.KVBucket) int {
	n := 0
	for _, asked := range []bool{ask.Counters, ask.Config, ask.Rate > 0} {
		if asked {
			n++
		}
	}
	return n
}

func limiterOptions(ask wire.KVBucket) []kv.LimiterOption {
	options := []kv.LimiterOption{
		kv.Rate(int64(min(ask.Rate, math.MaxInt64)), time.Duration(ask.Per)*time.Millisecond),
	}
	if ask.Burst > 0 {
		options = append(options, kv.Burst(int64(min(ask.Burst, math.MaxInt64))))
	}
	return options
}

func bucketOptions(ask wire.KVBucket) []kv.BucketOption {
	var options []kv.BucketOption
	if ask.DefaultTTL > 0 {
		options = append(options, kv.DefaultTTL(time.Duration(ask.DefaultTTL)*time.Millisecond))
	}
	if ask.Sliding > 0 {
		options = append(options, kv.Sliding(time.Duration(ask.Sliding)*time.Millisecond))
	}
	return options
}

func counterOptions(ask wire.KVBucket) []kv.CounterOption {
	var options []kv.CounterOption
	if ask.DefaultTTL > 0 {
		options = append(options, kv.DefaultTTL(time.Duration(ask.DefaultTTL)*time.Millisecond))
	}
	if ask.LoseAtMost > 0 {
		options = append(options, kv.LoseAtMost(time.Duration(ask.LoseAtMost)*time.Millisecond))
	}
	return options
}

// kvOne runs a call about one key, or one branch for clear
func kvOne(c *call, method wire.Method) error {
	var ask wire.KVCall
	if err := ask.Decode(c.request); err != nil {
		return err
	}
	handle, err := c.session.kvHandles.get(ask.Handle)
	if err != nil {
		return err
	}
	entry, err := handle.run(c.ctx, nil, method, ask)
	if err != nil {
		return err
	}
	return respond(c, entry)
}

// run is one call on a handle, inside tx when it is not nil
func (h *kvHandle) run(ctx context.Context, tx *kv.Tx, method wire.Method, ask wire.KVCall) (wire.KVEntry, error) {
	if err := h.only("values or counters"); err != nil {
		return wire.KVEntry{}, err
	}
	if h.counters != nil {
		counters := h.counters.Of(owners(ask.Owners)...)
		if tx != nil {
			counters = counters.WithTx(tx)
		}
		return runCounters(ctx, counters, method, ask)
	}
	options, err := callOptions(ask)
	if err != nil {
		return wire.KVEntry{}, err
	}
	bucket := h.values.Of(owners(ask.Owners)...)
	if tx != nil {
		bucket = bucket.WithTx(tx)
	}
	return runValues(ctx, bucket, method, ask, options)
}

func runValues(ctx context.Context, bucket *kv.Bucket[kv.Raw], method wire.Method, ask wire.KVCall,
	options []kv.Option,
) (wire.KVEntry, error) {
	switch method {
	case wire.KVGet:
		entry, found, err := bucket.GetEntry(ctx, ask.Key)
		return entryOf(entry, found), err
	case wire.KVHas:
		found, err := bucket.Has(ctx, ask.Key)
		return wire.KVEntry{Found: found}, err
	case wire.KVSet:
		return set(ctx, bucket, ask, options)
	case wire.KVDelete:
		return wire.KVEntry{}, bucket.Delete(ctx, ask.Key, options...)
	case wire.KVTake:
		value, found, err := bucket.Take(ctx, ask.Key, options...)
		return wire.KVEntry{Found: found, Value: valueOf(value)}, err
	case wire.KVTouch:
		found, err := bucket.Touch(ctx, ask.Key, options...)
		return wire.KVEntry{Found: found}, err
	case wire.KVClear:
		return wire.KVEntry{}, bucket.Clear(ctx)
	}
	return wire.KVEntry{}, fmt.Errorf("%w: %#04x is a counters' method; this handle holds values", tinystore.ErrInvalid,
		uint16(method))
}

func set(ctx context.Context, bucket *kv.Bucket[kv.Raw], ask wire.KVCall, options []kv.Option) (wire.KVEntry, error) {
	value := rawOf(ask.Value)
	if !ask.IfAbsent {
		entry, err := bucket.SetEntry(ctx, ask.Key, value, options...)
		return wire.KVEntry{Found: true, Version: versionOf(entry.Version), Expires: unixMillis(entry.ExpiresAt)}, err
	}
	entry, created, err := bucket.SetEntryIfAbsent(ctx, ask.Key, value, options...)
	answer := entryOf(entry, true)
	answer.Found = created
	if created {
		answer.Value = wire.KVValue{}
	}
	return answer, err
}

func runCounters(ctx context.Context, counters *kv.Counters, method wire.Method, ask wire.KVCall) (wire.KVEntry,
	error,
) {
	var n int64
	var err error
	switch method {
	case wire.KVGet:
		n, err = counters.Get(ctx, ask.Key)
	case wire.KVAdd:
		n, err = counters.Add(ctx, ask.Key, ask.N)
	case wire.KVMax:
		n, err = counters.Max(ctx, ask.Key, ask.N)
	case wire.KVDelete:
		return wire.KVEntry{}, counters.Delete(ctx, ask.Key)
	case wire.KVClear:
		return wire.KVEntry{}, counters.Clear(ctx)
	default:
		return wire.KVEntry{}, fmt.Errorf("%w: %#04x is a values' method; this handle holds counters",
			tinystore.ErrInvalid, uint16(method))
	}
	return wire.KVEntry{Found: true, Value: wire.KVValue{Kind: wire.KVInt, Int: n}}, err
}

// callOptions is a call's expiry and condition as the bucket takes them
func callOptions(ask wire.KVCall) ([]kv.Option, error) {
	var options []kv.Option
	if ask.TTL > 0 {
		options = append(options, kv.TTL(time.Duration(ask.TTL)*time.Millisecond))
	}
	if ask.ExpireAt != 0 {
		options = append(options, kv.ExpireAt(time.UnixMilli(ask.ExpireAt)))
	}
	if ask.IfVersion != nil {
		var version kv.Version
		if err := version.UnmarshalText(ask.IfVersion); err != nil {
			return nil, err
		}
		options = append(options, kv.IfVersion(version))
	}
	return options, nil
}

func owners(texts []string) []any {
	named := make([]any, len(texts))
	for i, text := range texts {
		named[i] = text
	}
	return named
}

func entryOf(entry kv.Entry[kv.Raw], found bool) wire.KVEntry {
	if !found {
		return wire.KVEntry{}
	}
	return wire.KVEntry{
		Found: true, Value: valueOf(entry.Value), Version: versionOf(entry.Version),
		Expires: unixMillis(entry.ExpiresAt),
	}
}

func valueOf(raw kv.Raw) wire.KVValue {
	return wire.KVValue{Kind: wire.KVKind(raw.Kind), Int: raw.Int, Bytes: raw.Bytes}
}

func rawOf(value wire.KVValue) kv.Raw {
	return kv.Raw{Kind: kv.RawKind(value.Kind), Int: value.Int, Bytes: value.Bytes}
}

// versionOf is a version as the wire carries it: its text, which the client
// compares only for equality and gives back
func versionOf(version kv.Version) []byte {
	if text := version.String(); text != "" {
		return []byte(text)
	}
	return nil
}

// kvScan is a download: a page of a branch's own keys, an entry a DATA, and a
// last DATA saying where the next page begins
func kvScan(c *call) error {
	var ask wire.KVCall
	if err := ask.Decode(c.request); err != nil {
		return err
	}
	handle, err := c.session.kvHandles.get(ask.Handle)
	if err != nil {
		return err
	}
	if handle.values == nil {
		return fmt.Errorf("%w: kv: %q is not a bucket of values, which scan reads", tinystore.ErrInvalid, handle.name)
	}
	query := kv.Query{After: ask.After, Limit: int(min(ask.Limit, math.MaxInt32))}
	page, err := handle.values.Of(owners(ask.Owners)...).Scan(c.ctx, query)
	if err != nil {
		return err
	}
	release, err := c.holdAnswer(func() int64 { return weighEntries(page.Entries) })
	if err != nil {
		return err
	}
	defer release()

	if err = begin(c, wire.Empty{}); err != nil {
		return err
	}
	for _, entry := range page.Entries {
		scanned := entryOf(entry, true)
		scanned.Found, scanned.Key = false, entry.Key
		if err = item(c, scanned); err != nil {
			return err
		}
	}
	return trailer(c, wire.KVPage{More: page.More, After: page.Next.After})
}

// kvBatch runs its calls in one transaction: a failure of any rolls back all
func kvBatch(c *call) error {
	var ask wire.KVCalls
	if err := ask.Decode(c.request); err != nil {
		return err
	}
	handles, state, err := batchHandles(c.session, ask)
	if err != nil {
		return err
	}
	results := wire.KVResults{Entries: make([]wire.KVEntry, len(ask.Calls))}
	err = state.Tx(c.ctx, func(tx *kv.Tx) error {
		for i, op := range ask.Calls {
			if !batched[op.Method] {
				return opFailed(i, fmt.Errorf("%w: %#04x in a batch", tinystore.ErrInvalid, uint16(op.Method)))
			}
			entry, opErr := handles[i].run(c.ctx, tx, op.Method, op.KVCall)
			if opErr != nil {
				return opFailed(i, opErr)
			}
			results.Entries[i] = entry
		}
		return nil
	})
	if err != nil {
		return err
	}
	return respond(c, results)
}

// kvView runs its reads from one snapshot
func kvView(c *call) error {
	var ask wire.KVCalls
	if err := ask.Decode(c.request); err != nil {
		return err
	}
	handles, state, err := batchHandles(c.session, ask)
	if err != nil {
		return err
	}
	results := wire.KVResults{Entries: make([]wire.KVEntry, len(ask.Calls))}
	err = state.View(c.ctx, func(tx *kv.Tx) error {
		for i, op := range ask.Calls {
			if op.Method != wire.KVGet && op.Method != wire.KVHas {
				return opFailed(i, fmt.Errorf("%w: %#04x in a view, which reads", tinystore.ErrInvalid,
					uint16(op.Method)))
			}
			entry, opErr := handles[i].run(c.ctx, tx, op.Method, op.KVCall)
			if opErr != nil {
				return opFailed(i, opErr)
			}
			results.Entries[i] = entry
		}
		return nil
	})
	if err != nil {
		return err
	}
	return respond(c, results)
}

var batched = map[wire.Method]bool{
	wire.KVGet: true, wire.KVHas: true, wire.KVSet: true, wire.KVDelete: true, wire.KVTake: true,
	wire.KVTouch: true, wire.KVAdd: true, wire.KVMax: true, wire.KVClear: true,
}

func batchHandles(s *session, ask wire.KVCalls) ([]*kvHandle, *kv.Store, error) {
	handles := make([]*kvHandle, len(ask.Calls))
	for i, op := range ask.Calls {
		handle, err := s.kvHandles.get(op.Handle)
		if err != nil {
			return nil, nil, opFailed(i, err)
		}
		handles[i] = handle
	}
	if len(handles) == 0 {
		return nil, nil, fmt.Errorf("%w: a batch of no calls", tinystore.ErrInvalid)
	}
	return handles, handles[0].state, nil
}

// opError is a batch's failure in one of its calls, which it names
type opError struct {
	index int
	err   error
}

func (e *opError) Error() string { return fmt.Sprintf("call %d of the batch: %v", e.index, e.err) }
func (e *opError) Unwrap() error { return e.err }

func opFailed(index int, err error) error {
	return &opError{index: index, err: err}
}

func (e *opError) what() map[string]string {
	what := whatOf(e.err)
	if what == nil {
		what = map[string]string{}
	}
	what["call"] = strconv.Itoa(e.index)
	return what
}

// only refuses a call that a config or a limiter does not take: a config is
// changed by configure and read by watch, and a limiter answers allow alone
func (h *kvHandle) only(what string) error {
	switch {
	case h.config != nil:
		return fmt.Errorf("%w: kv: %q is a config, which takes configure and watch, not a call of %s",
			tinystore.ErrInvalid, h.name, what)
	case h.limiter != nil:
		return fmt.Errorf("%w: kv: %q is a limiter, which takes allow, not a call of %s",
			tinystore.ErrInvalid, h.name, what)
	}
	return nil
}

// kvAllow asks a limiter for n requests of a key, one when n is absent
func kvAllow(c *call) error {
	var ask wire.KVCall
	if err := ask.Decode(c.request); err != nil {
		return err
	}
	handle, err := c.session.kvHandles.get(ask.Handle)
	if err != nil {
		return err
	}
	if handle.limiter == nil {
		return fmt.Errorf("%w: kv: %q is not a limiter, which allow asks", tinystore.ErrInvalid, handle.name)
	}
	allowance, err := handle.limiter.Of(owners(ask.Owners)...).AllowN(c.ctx, ask.Key, max(ask.N, 1))
	if err != nil {
		return err
	}
	wait := (allowance.RetryAfter + time.Millisecond - 1) / time.Millisecond
	return respond(c, wire.KVAllowance{
		OK: allowance.OK, Left: uint64(allowance.Left), RetryAfter: uint64(wait), //nolint:gosec // never negative
	})
}

// kvConfigure keeps and forgets a config's fields in one transaction
func kvConfigure(c *call) error {
	var ask wire.KVConfigChange
	if err := ask.Decode(c.request); err != nil {
		return err
	}
	handle, err := c.session.kvHandles.get(ask.Handle)
	if err != nil {
		return err
	}
	if handle.config == nil {
		return fmt.Errorf("%w: kv: %q is not a config, which configure changes", tinystore.ErrInvalid, handle.name)
	}
	set := make(map[string]json.RawMessage, len(ask.Set)/2)
	for i := 0; i+1 < len(ask.Set); i += 2 {
		set[ask.Set[i]] = json.RawMessage(ask.Set[i+1])
	}
	if err = handle.config.Change(c.ctx, set, ask.Reset); err != nil {
		return err
	}
	return respond(c, wire.Empty{})
}

// kvWatch sends a config's kept fields now and after each change, until the
// client cancels the stream or the server closes
func kvWatch(c *call) error {
	var ask wire.KVCall
	if err := ask.Decode(c.request); err != nil {
		return err
	}
	handle, err := c.session.kvHandles.get(ask.Handle)
	if err != nil {
		return err
	}
	if handle.config == nil {
		return fmt.Errorf("%w: kv: %q is not a config, which watch follows", tinystore.ErrInvalid, handle.name)
	}
	c.follow()
	if err = begin(c, wire.Empty{}); err != nil {
		return err
	}
	for kept, changes := range handle.config.Watch(c.ctx) {
		fields := make([]string, 0, 2*len(kept))
		for _, path := range slices.Sorted(maps.Keys(kept)) {
			fields = append(fields, path, string(kept[path]))
		}
		if err = item(c, wire.KVKept{Changes: uint64(changes), Fields: fields}); err != nil { //nolint:gosec // a count
			return err
		}
	}
	return context.Cause(c.ctx)
}
