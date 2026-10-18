package metrics

import (
	"context"
	"crypto/rand"
	"encoding/json"
	"math"
	"sort"
	"sync"
	"time"

	"github.com/tinyshed/tinystore/internal/core"
	"github.com/tinyshed/tinystore/internal/wire"
)

// What changed goes to the store's totals on a stream of its own, a batch a
// second, each numbered in this run's session, so that the store counts it
// once: a batch whose answer a lost connection took goes again on the next,
// and a store that started again is sent nothing it may have counted.

const (
	sendEvery = time.Second
	// readEvery is how often a gauge's function is read.
	readEvery = 15 * time.Second
	// mostUnanswered is the batches sent and not yet answered, past which changes gather for the next.
	mostUnanswered = 16
	mostChanges    = 1000
	// lastAnswer is how long the store's close waits for the answer that ends the stream.
	lastAnswer = 5 * time.Second
)

// declared is an instrument as the stream declares it, and its sets of labels.
type declared struct {
	in          *instruments
	id          uint64
	name        string
	form        form
	unit, help  *string
	keep        *uint64
	bounds      []float64
	exponential bool
	sets        map[string]*set
	overflow    *set
	// refused says the store refused it: its calls change nothing, said once
	refused bool
	// reads are a gauge's functions, one a set of labels; nil for any other instrument
	reads map[string]*gaugeRead
}

type gaugeRead struct {
	labels  Labels
	read    func(context.Context) (float64, error)
	reading bool
	failed  string
}

// set is a set of labels of an instrument: its id on the stream, and a gauge's value.
type set struct {
	id         uint64
	instrument *declared
	labels     Labels
	overflow   bool
	touched    time.Time
	value      *float64
}

// change is what a set of labels changed since the batch before.
type change struct {
	add, value   *float64
	count        uint64
	sum, longest float64
	buckets      []uint64
	bins         map[string]uint64
}

type batch struct {
	number  uint64
	sets    []*set
	changes []*change
	flush   bool
	waiters []chan error
	sentOn  *opened
}

// opened is a stream of the instruments, and what it declared.
type opened struct {
	stream      *core.Stream
	instruments map[uint64]bool
	sets        map[uint64]bool
	// listened is closed once what reads the store's answers ends, with the stream
	listened chan struct{}
}

// instruments are a store's instruments, and what sends their changes.
type instruments struct {
	store    *core.Store
	session  []byte
	failures core.FailureLog
	// sending is held by the send that runs, so that batches leave in order
	sending  sync.Mutex
	starting sync.Once

	mu         sync.Mutex
	declared   map[string]*declared
	byID       map[uint64]*declared
	pending    map[*set]*change
	unanswered []*batch
	answered   uint64
	next       uint64
	ids        uint64
	opened     *opened
	stopped    bool
	quit       chan struct{}
	unreported error
}

func newInstruments(store *core.Store) *instruments {
	session := make([]byte, 16)
	_, _ = rand.Read(session)
	return &instruments{
		store: store, session: session, failures: core.FailureLog{Work: "metrics instruments"},
		declared: map[string]*declared{}, byID: map[uint64]*declared{}, pending: map[*set]*change{},
		next: 1, quit: make(chan struct{}),
	}
}

// declare is an instrument of name, made the first time; another form or
// other options than it has are refused.
func (in *instruments) declare(name string, f form, opts []Option, read bool) (*declared, error) {
	t, err := toldOf(name, f, opts)
	if err != nil {
		return nil, err
	}
	in.starting.Do(func() {
		in.store.OnClose(core.StopInstruments, in.stop)
		go in.tick()
	})
	in.mu.Lock()
	defer in.mu.Unlock()
	if held, ok := in.declared[name]; ok {
		alike := held.form == f && sameValue(held.unit, t.unit) && sameValue(held.help, t.help) && sameValue(held.keep, t.keep) &&
			sameBounds(held.bounds, t.bounds) && held.exponential == t.exponential && (held.reads != nil) == read
		if !alike {
			return nil, core.Errorf(core.Invalid, "metrics %s: another instrument or other options than it has while the store is open", name)
		}
		return held, nil
	}
	in.ids++
	d := &declared{
		in: in, id: in.ids, name: name, form: f, unit: t.unit, help: t.help, keep: t.keep,
		bounds: t.bounds, exponential: t.exponential, sets: map[string]*set{},
	}
	if read {
		d.reads = map[string]*gaugeRead{}
	}
	in.declared[name], in.byID[d.id] = d, d
	return d, nil
}

func sameValue[T comparable](a, b *T) bool {
	return a == nil && b == nil || a != nil && b != nil && *a == *b
}

func sameBounds(a, b []float64) bool {
	if len(a) != len(b) || (a == nil) != (b == nil) {
		return false
	}
	for i := range a {
		if a[i] != b[i] {
			return false
		}
	}
	return true
}

func keyOf(labels Labels) string {
	names := make([]string, 0, len(labels))
	for name := range labels {
		names = append(names, name)
	}
	sort.Strings(names)
	pairs := make([][2]string, len(names))
	for i, name := range names {
		pairs[i] = [2]string{name, labels[name]}
	}
	key, _ := json.Marshal(pairs)
	return string(key)
}

// setOf is the set of labels of an instrument, made the first time; past
// the sets it holds, its __overflow. The caller holds mu.
func (in *instruments) setOf(d *declared, labels Labels) *set {
	key := keyOf(labels)
	if held, ok := d.sets[key]; ok {
		return held
	}
	if len(d.sets) >= mostSets {
		if d.overflow == nil {
			in.ids++
			d.overflow = &set{id: in.ids, instrument: d, labels: Labels{}, overflow: true, touched: time.Now()}
		}
		return d.overflow
	}
	in.ids++
	made := &set{id: in.ids, instrument: d, labels: labels, touched: time.Now()}
	d.sets[key] = made
	return made
}

// changeOf is what a call changes of a set, kept for the next batch. The caller holds mu.
func (in *instruments) changeOf(s *set) *change {
	s.touched = time.Now()
	if found, ok := in.pending[s]; ok {
		return found
	}
	made := &change{}
	if s.instrument.bounds != nil {
		made.buckets = make([]uint64, len(s.instrument.bounds)+1)
	}
	if s.instrument.exponential {
		made.bins = map[string]uint64{}
	}
	in.pending[s] = made
	return made
}

func (in *instruments) add(d *declared, labels Labels, n float64) {
	in.mu.Lock()
	defer in.mu.Unlock()
	if d.refused {
		return
	}
	s := in.setOf(d, labels)
	added := n
	if found, ok := in.pending[s]; ok && found.add != nil {
		added += *found.add
	}
	if math.IsInf(added, 0) {
		refuse(d.name, core.Errorf(core.Invalid, "a sum past the largest number a send carries"))
		return
	}
	in.changeOf(s).add = &added
}

func (in *instruments) set(d *declared, labels Labels, n float64, by bool) error {
	in.mu.Lock()
	defer in.mu.Unlock()
	if d.refused {
		return nil
	}
	s := in.setOf(d, labels)
	value := n
	if by && s.value != nil {
		value += *s.value
	}
	if math.IsInf(value, 0) {
		return core.Errorf(core.Invalid, "a gauge moved to %v, where a finite number is", value)
	}
	s.value = &value
	in.changeOf(s).value = &value
	return nil
}

// measure counts a duration or a value into a timer's or a histogram's
// change: its count, sum, longest and bucket.
func (in *instruments) measure(d *declared, labels Labels, value float64) {
	in.mu.Lock()
	defer in.mu.Unlock()
	if d.refused {
		return
	}
	s := in.setOf(d, labels)
	if found, ok := in.pending[s]; ok && math.IsInf(found.sum+value, 0) {
		refuse(d.name, core.Errorf(core.Invalid, "a sum past the largest number a send carries"))
		return
	}
	c := in.changeOf(s)
	c.count++
	c.sum += value
	if c.count == 1 || value > c.longest {
		c.longest = value
	}
	if c.buckets != nil {
		bucket := len(d.bounds)
		for i, bound := range d.bounds {
			if value <= bound {
				bucket = i
				break
			}
		}
		c.buckets[bucket]++
	}
	if c.bins != nil {
		c.bins[binOf(value)]++
	}
}

// flush sends what changed now and has the store write its totals, and
// says whether there was anything to send.
func (in *instruments) flush(ctx context.Context) (bool, error) {
	in.mu.Lock()
	none := len(in.declared) == 0
	in.mu.Unlock()
	if none {
		return false, nil
	}
	in.readGauges(ctx, true)
	done := make(chan error, 1)
	in.mu.Lock()
	made := in.batches(true)
	made[len(made)-1].waiters = append(made[len(made)-1].waiters, done)
	in.mu.Unlock()
	go in.pump()
	select {
	case err := <-done:
		return true, err
	case <-ctx.Done():
		return true, ctx.Err()
	}
}

// tick sends what changed every second, reads the gauges' functions every
// 15, and lets go of the sets nothing changed for an hour.
func (in *instruments) tick() {
	ticker := time.NewTicker(sendEvery)
	defer ticker.Stop()
	lastRead := time.Now()
	for {
		select {
		case <-in.quit:
			return
		case <-ticker.C:
		}
		if time.Since(lastRead) >= readEvery {
			lastRead = time.Now()
			go in.readGauges(context.Background(), false)
		}
		in.mu.Lock()
		in.forgetIdle()
		send := len(in.pending) > 0 && len(in.unanswered) < mostUnanswered
		if send {
			in.batches(false)
		}
		in.mu.Unlock()
		if send {
			in.pump()
		}
	}
}

// forgetIdle lets go of a counter's, a timer's or a histogram's sets that
// nothing changed for an hour; sent again, each starts a new run with its
// zero. The caller holds mu.
func (in *instruments) forgetIdle() {
	for _, d := range in.declared {
		if d.form == formGauge {
			continue
		}
		for key, s := range d.sets {
			if _, waits := in.pending[s]; !waits && time.Since(s.touched) >= idle {
				delete(d.sets, key)
			}
		}
	}
}

// batches is what waits, as batches of mostChanges changes each; with
// flush, the last asks the store to write. The caller holds mu.
func (in *instruments) batches(flush bool) []*batch {
	var made []*batch
	sets := make([]*set, 0, len(in.pending))
	for s := range in.pending {
		sets = append(sets, s)
	}
	sort.Slice(sets, func(i, j int) bool { return sets[i].id < sets[j].id })
	for start := 0; start < len(sets) || flush && len(made) == 0; start += mostChanges {
		end := min(start+mostChanges, len(sets))
		b := &batch{number: in.next, flush: flush && end == len(sets)}
		for _, s := range sets[start:end] {
			b.sets, b.changes = append(b.sets, s), append(b.changes, in.pending[s])
		}
		in.next++
		made = append(made, b)
	}
	in.pending = map[*set]*change{}
	in.unanswered = append(in.unanswered, made...)
	return made
}

// pump sends every unanswered batch not gone on the stream open now, in
// order; a connection lost on the way is dialled again, as a call's is.
func (in *instruments) pump() {
	in.sending.Lock()
	defer in.sending.Unlock()
	ctx := context.Background()
	for tries := 0; ; tries++ {
		o, err := in.stream(ctx)
		if err == nil {
			err = in.sendOn(ctx, o)
		}
		if err == nil {
			in.failures.Succeeded()
			return
		}
		if lost, _ := core.IsConnectionLost(err); lost && tries < 2 {
			continue
		}
		in.failures.Failed(err)
		in.mu.Lock()
		in.fail(err)
		in.mu.Unlock()
		return
	}
}

func (in *instruments) sendOn(ctx context.Context, o *opened) error {
	for {
		in.mu.Lock()
		var next *batch
		for _, b := range in.unanswered {
			if b.sentOn != o {
				next = b
				break
			}
		}
		if next == nil {
			in.mu.Unlock()
			return nil
		}
		next.sentOn = o
		body := in.encode(next, o)
		in.mu.Unlock()
		if err := o.stream.Send(ctx, body, false); err != nil {
			return err
		}
	}
}

// fail fails every flush waiting with err; the batches wait for the next send. The caller holds mu.
func (in *instruments) fail(err error) {
	for _, b := range in.unanswered {
		for _, waiter := range b.waiters {
			waiter <- errorOf(err)
		}
		b.waiters = nil
	}
}

// failWith fails the flushes that wait with err, or the next flush when none does. The caller holds mu.
func (in *instruments) failWith(waiters []chan error, err error) {
	if len(waiters) == 0 && in.unreported == nil {
		in.unreported = err
	}
	for _, waiter := range waiters {
		waiter <- err
	}
}

func errorOf(err error) error {
	if core.KindOf(err) != "" {
		return err
	}
	return core.Wrap(core.Unavailable, err, "metrics instruments")
}

// stream is the stream open now, or one opened and resumed: the store says
// what it applied of the session. Only pump calls it, so one opens at a time.
func (in *instruments) stream(ctx context.Context) (*opened, error) {
	in.mu.Lock()
	o := in.opened
	after := in.answered
	in.mu.Unlock()
	if o != nil && !o.stream.Finished() {
		return o, nil
	}
	c, err := in.store.Link.Connection(ctx)
	if err != nil {
		return nil, err
	}
	st, err := c.Session.Open(ctx, wire.MethodMetricsInstruments, wire.Encode(&wire.MetricsSession{Id: in.session, After: after}), false)
	if err != nil {
		return nil, err
	}
	head, err := st.Next(ctx)
	if err != nil {
		st.Cancel()
		return nil, err
	}
	var ack wire.MetricsAck
	if err := decode(head.Body, &ack); err != nil {
		st.Cancel()
		return nil, err
	}
	o = &opened{stream: st, instruments: map[uint64]bool{}, sets: map[uint64]bool{}, listened: make(chan struct{})}
	in.mu.Lock()
	in.resume(ack)
	in.opened = o
	in.mu.Unlock()
	go in.listen(o)
	return o, nil
}

// resume takes up where the store is. A batch it applied is done; one sent
// that it may have applied, when it knows no session, is dropped rather than
// counted twice, and those never sent are numbered on. Every gauge's value
// goes again, since a gauge left with the last connection. The caller holds mu.
func (in *instruments) resume(ack wire.MetricsAck) {
	if ack.Known {
		in.answer(ack)
	} else {
		var lost, kept []*batch
		for _, b := range in.unanswered {
			if b.sentOn != nil {
				lost = append(lost, b)
			} else {
				kept = append(kept, b)
			}
		}
		in.unanswered = kept
		if len(lost) > 0 {
			changes := 0
			var waiters []chan error
			for _, b := range lost {
				changes += len(b.changes)
				waiters = append(waiters, b.waiters...)
			}
			core.SayOwn("WARN", "metrics instruments dropped what a store that started again may have counted",
				"batches", len(lost), "changes", changes)
			in.failWith(waiters, core.Errorf(core.OutcomeUnknown, "the store started again: %d batches of instruments it may "+
				"have counted before it stopped are dropped rather than counted twice", len(lost)))
		}
		in.answered = ack.Applied
		in.renumber()
	}
	for _, d := range in.declared {
		if d.form != formGauge {
			continue
		}
		for _, s := range d.sets {
			if s.value != nil {
				value := *s.value
				in.changeOf(s).value = &value
			}
		}
	}
}

// renumber numbers what waits after the last batch the store answered. The caller holds mu.
func (in *instruments) renumber() {
	in.next = in.answered + 1
	for _, b := range in.unanswered {
		b.number = in.next
		in.next++
	}
}

// listen reads the store's answers on a stream until it ends; what it had
// not answered goes again on another.
func (in *instruments) listen(o *opened) {
	defer close(o.listened)
	for {
		event, err := o.stream.Next(context.Background())
		if err != nil {
			in.mu.Lock()
			stopped := in.stopped
			if !stopped && core.KindOf(err) == core.Invalid {
				in.drop(err)
			}
			in.mu.Unlock()
			if !stopped {
				in.failures.Failed(err)
			}
			break
		}
		o.stream.Consumed(len(event.Body))
		var ack wire.MetricsAck
		if err := decode(event.Body, &ack); err == nil {
			in.mu.Lock()
			in.answer(ack)
			in.mu.Unlock()
		}
		if event.End {
			break
		}
	}
	in.mu.Lock()
	if in.opened == o {
		in.opened = nil
	}
	again := !in.stopped && len(in.unanswered) > 0
	in.mu.Unlock()
	if again {
		go in.pump()
	}
}

// drop drops the batch the store refused, which it was to apply next and
// would refuse again, said once. The caller holds mu.
func (in *instruments) drop(err error) {
	if len(in.unanswered) == 0 {
		return
	}
	dropped := in.unanswered[0]
	in.unanswered = in.unanswered[1:]
	core.SayOwn("WARN", "metrics instruments dropped a batch the store refused", "changes", len(dropped.changes), "error", err)
	in.failWith(dropped.waiters, err)
	in.renumber()
}

// answer takes an answer: the instruments the store refused, then the
// batches it applied. The caller holds mu.
func (in *instruments) answer(ack wire.MetricsAck) {
	for _, refused := range ack.Refused {
		d := in.byID[refused.Instrument]
		if d == nil || d.refused {
			continue
		}
		d.refused = true
		core.SayOwn("WARN", "metrics instrument refused", "instrument", d.name, "error", refused.Error)
		if in.unreported == nil {
			in.unreported = core.Errorf(core.Invalid, "metrics %s: the store refused it: %s", d.name, refused.Error)
		}
	}
	in.answered = max(in.answered, ack.Applied)
	for len(in.unanswered) > 0 && in.unanswered[0].number <= ack.Applied {
		done := in.unanswered[0]
		in.unanswered = in.unanswered[1:]
		for _, waiter := range done.waiters {
			failed := in.unreported
			in.unreported = nil
			waiter <- failed
		}
	}
}

// readGauges calls each gauge's function whose last call ended; a value it
// gives goes with the next batch. With wait, it returns once each call ended.
func (in *instruments) readGauges(ctx context.Context, wait bool) {
	type reading struct {
		d *declared
		g *gaugeRead
	}
	var reads []reading
	in.mu.Lock()
	for _, d := range in.declared {
		if d.refused {
			continue
		}
		for _, g := range d.reads {
			if !g.reading {
				g.reading = true
				reads = append(reads, reading{d, g})
			}
		}
	}
	in.mu.Unlock()
	var running sync.WaitGroup
	for _, r := range reads {
		running.Add(1)
		go func() {
			defer running.Done()
			in.readGauge(ctx, r.d, r.g)
		}()
	}
	if wait {
		running.Wait()
	}
}

func (in *instruments) readGauge(ctx context.Context, d *declared, g *gaugeRead) {
	value, err := g.read(ctx)
	if err == nil && (math.IsNaN(value) || math.IsInf(value, 0)) {
		err = core.Errorf(core.Invalid, "a gauge's function gave %v, where a finite number is", value)
	}
	in.mu.Lock()
	defer in.mu.Unlock()
	g.reading = false
	if err != nil {
		if err.Error() != g.failed {
			labels, _ := json.Marshal(g.labels)
			core.SayOwn("WARN", "metrics gauge read failed", "instrument", d.name, "labels", string(labels), "error", err)
			g.failed = err.Error()
		}
		return
	}
	g.failed = ""
	s := in.setOf(d, g.labels)
	s.value = &value
	read := value
	in.changeOf(s).value = &read
}

// encode is a batch's bytes for a stream: the instruments and sets it names
// that the stream has not declared. The caller holds mu.
func (in *instruments) encode(b *batch, o *opened) []byte {
	message := wire.MetricsBatch{Number: b.number, Flush: b.flush}
	for i, s := range b.sets {
		d := s.instrument
		if d.refused {
			continue
		}
		if !o.instruments[d.id] {
			o.instruments[d.id] = true
			declared := wire.MetricsInstrument{
				Number: d.id, Name: d.name, Form: string(d.form), Unit: d.unit, Help: d.help, Keep: d.keep, Exponential: d.exponential,
			}
			if d.bounds != nil {
				declared.Bounds = &d.bounds
			}
			message.Instruments = append(message.Instruments, declared)
		}
		if !o.sets[s.id] {
			o.sets[s.id] = true
			message.Sets = append(message.Sets, wire.MetricsSet{Number: s.id, Instrument: d.id, Labels: s.labels, Overflow: s.overflow})
		}
		c := b.changes[i]
		message.Changes = append(message.Changes, wire.MetricsChange{
			Set: s.id, Add: c.add, Value: c.value, Count: c.count, Sum: c.sum, Longest: c.longest, Buckets: c.buckets, Bins: c.bins,
		})
	}
	return wire.Encode(&message)
}

// stop stops the timers, sends what is left, and ends the stream, as the
// store closes: the store ends the stream once it answered the last DATA,
// and a connection closed before then would leave a server draining it.
func (in *instruments) stop() {
	in.mu.Lock()
	if in.stopped {
		in.mu.Unlock()
		return
	}
	in.stopped = true
	close(in.quit)
	none := len(in.declared) == 0
	in.mu.Unlock()
	if none {
		return
	}
	ctx, cancel := context.WithTimeout(context.Background(), 2*lastAnswer)
	defer cancel()
	if _, err := in.flush(ctx); err != nil {
		in.failures.Failed(err)
	}
	in.sending.Lock()
	in.mu.Lock()
	o := in.opened
	in.mu.Unlock()
	if o != nil && o.stream.Send(ctx, nil, true) == nil {
		select {
		case <-o.listened:
		case <-time.After(lastAnswer):
		}
	}
	in.sending.Unlock()
}
