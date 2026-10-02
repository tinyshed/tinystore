package wire

// The jobs engine's methods, docs/wire.md#jobs.
const (
	JobsOpen    Method = 0x0201
	JobsEnqueue Method = 0x0202
	JobsUpdate  Method = 0x0203
	JobsCancel  Method = 0x0204
	JobsGet     Method = 0x0205
	JobsClaim   Method = 0x0206
	JobsSettle  Method = 0x0207
	JobsScan    Method = 0x0208
	JobsWork    Method = 0x0209
	JobsWatch   Method = 0x020a
	JobsStep    Method = 0x020b
	JobsKeep    Method = 0x020c
)

// JobsQueue is jobs.open's request: a queue by name with its policy, or a
// schedule, a queue holding one repeating job under its name. A duration is
// milliseconds, zero for the queue's default.
type JobsQueue struct {
	Name         string
	Lease        int64
	MaxAttempts  uint64
	BackoffFirst int64
	BackoffMost  int64
	MaxWaiting   uint64
	KeepFailed   int64
	KeepDone     int64
	Schedule     *Repeat
	MaxRunning   uint64
}

func (q JobsQueue) Append(dst []byte) []byte {
	m := BeginMap(dst)
	m.Str(1, q.Name)
	optionalInt(&m, 2, q.Lease)
	optionalUint(&m, 3, q.MaxAttempts)
	optionalInt(&m, 4, q.BackoffFirst)
	optionalInt(&m, 5, q.BackoffMost)
	optionalUint(&m, 6, q.MaxWaiting)
	optionalInt(&m, 7, q.KeepFailed)
	optionalInt(&m, 8, q.KeepDone)
	if q.Schedule != nil {
		m.Key(9)
		m.SetBuf(q.Schedule.Append(m.Buf()))
	}
	optionalUint(&m, 10, q.MaxRunning)
	return m.End()
}

func (q *JobsQueue) Decode(body []byte) error {
	d := NewDecoder(body)
	for key := range d.Fields() {
		switch key {
		case 1:
			q.Name = d.Str()
		case 2:
			q.Lease = d.Duration()
		case 3:
			q.MaxAttempts = d.Uint()
		case 4:
			q.BackoffFirst = d.Duration()
		case 5:
			q.BackoffMost = d.Duration()
		case 6:
			q.MaxWaiting = d.Uint()
		case 7:
			q.KeepFailed = d.Duration()
		case 8:
			q.KeepDone = d.Duration()
		case 9:
			q.Schedule = &Repeat{}
			q.Schedule.decode(&d)
		case 10:
			q.MaxRunning = d.Uint()
		}
	}
	return d.End()
}

func optionalInt(m *Map, key uint64, v int64) {
	if v != 0 {
		m.Int(key, v)
	}
}

func optionalUint(m *Map, key, v uint64) {
	if v != 0 {
		m.Uint(key, v)
	}
}

func optionalStr(m *Map, key uint64, s string) {
	if s != "" {
		m.Str(key, s)
	}
}

// Repeat is when a job runs again: a cron expression in a zone by its name,
// or every so many milliseconds.
type Repeat struct {
	Cron  string
	Zone  string
	Every int64
}

func (r Repeat) Append(dst []byte) []byte {
	m := BeginMap(dst)
	optionalStr(&m, 1, r.Cron)
	optionalStr(&m, 2, r.Zone)
	optionalInt(&m, 3, r.Every)
	return m.End()
}

func (r *Repeat) decode(d *Decoder) {
	for key := range d.Fields() {
		switch key {
		case 1:
			r.Cron = d.Str()
		case 2:
			r.Zone = d.Str()
		case 3:
			r.Every = d.Duration()
		}
	}
}

// JobsJob is a job to enqueue: its value as JSON text, its key, when it runs,
// at a time or after a while, and how it repeats.
type JobsJob struct {
	Value  string
	Key    string
	At     int64 // unix milliseconds
	After  int64 // milliseconds
	Repeat *Repeat
}

func (j JobsJob) appendFields(m *Map) {
	m.Str(1, j.Value)
	optionalStr(m, 2, j.Key)
	optionalInt(m, 3, j.At)
	optionalInt(m, 4, j.After)
	if j.Repeat != nil {
		m.Key(5)
		m.SetBuf(j.Repeat.Append(m.Buf()))
	}
}

func (j *JobsJob) decodeField(d *Decoder, key uint64) {
	switch key {
	case 1:
		j.Value = d.Str()
	case 2:
		j.Key = d.Str()
	case 3:
		j.At = d.Int()
	case 4:
		j.After = d.Duration()
	case 5:
		j.Repeat = &Repeat{}
		j.Repeat.decode(d)
	}
}

// JobsBatch is jobs.enqueue's request: jobs one transaction adds, all or
// none.
type JobsBatch struct {
	Handle uint64
	Jobs   []JobsJob
}

func (e JobsBatch) Append(dst []byte) []byte {
	m := BeginMap(dst)
	m.Uint(1, e.Handle)
	m.Key(2)
	buf := AppendArray(m.Buf(), len(e.Jobs))
	for _, job := range e.Jobs {
		fields := BeginMap(buf)
		job.appendFields(&fields)
		buf = fields.End()
	}
	m.SetBuf(buf)
	return m.End()
}

func (e *JobsBatch) Decode(body []byte) error {
	d := NewDecoder(body)
	for key := range d.Fields() {
		switch key {
		case 1:
			e.Handle = d.Uint()
		case 2:
			for range d.Items() {
				var job JobsJob
				for field := range d.Fields() {
					job.decodeField(&d, field)
				}
				e.Jobs = append(e.Jobs, job)
			}
		}
	}
	return d.End()
}

// JobsChange is jobs.update's request: a job by its key, given a new value,
// and a new time or repeat when it names one.
type JobsChange struct {
	Handle uint64
	JobsJob
}

func (u JobsChange) Append(dst []byte) []byte {
	m := BeginMap(dst)
	u.appendFields(&m)
	m.Uint(6, u.Handle)
	return m.End()
}

func (u *JobsChange) Decode(body []byte) error {
	d := NewDecoder(body)
	for key := range d.Fields() {
		if key == 6 {
			u.Handle = d.Uint()
			continue
		}
		u.decodeField(&d, key)
	}
	return d.End()
}

// JobsKey is the request of jobs.cancel and jobs.get: a job by its key.
type JobsKey struct {
	Handle uint64
	Key    string
}

func (k JobsKey) Append(dst []byte) []byte {
	m := BeginMap(dst)
	m.Uint(1, k.Handle)
	m.Str(2, k.Key)
	return m.End()
}

func (k *JobsKey) Decode(body []byte) error {
	d := NewDecoder(body)
	for key := range d.Fields() {
		switch key {
		case 1:
			k.Handle = d.Uint()
		case 2:
			k.Key = d.Str()
		}
	}
	return d.End()
}

// JobsAnswer names a step of a held job's run: jobs.step's request, for the
// answer the run kept under it, and jobs.keep's, with the answer to keep. Job
// is the number the job was held under, by a claim or a work stream.
type JobsAnswer struct {
	Job  uint64
	Name string
	// Answer is jobs.keep's: the step's answer as JSON.
	Answer string
}

func (a JobsAnswer) Append(dst []byte) []byte {
	m := BeginMap(dst)
	m.Uint(1, a.Job)
	m.Str(2, a.Name)
	optionalStr(&m, 3, a.Answer)
	return m.End()
}

func (a *JobsAnswer) Decode(body []byte) error {
	d := NewDecoder(body)
	for key := range d.Fields() {
		switch key {
		case 1:
			a.Job = d.Uint()
		case 2:
			a.Name = d.Str()
		case 3:
			a.Answer = d.Str()
		}
	}
	return d.End()
}

// JobsKept is jobs.step's answer: the answer a step kept, found false for a
// step its run has not kept.
type JobsKept struct {
	Found  bool
	Answer string
}

func (k JobsKept) Append(dst []byte) []byte {
	m := BeginMap(dst)
	if k.Found {
		m.Bool(1, true)
	}
	optionalStr(&m, 2, k.Answer)
	return m.End()
}

func (k *JobsKept) Decode(body []byte) error {
	d := NewDecoder(body)
	for key := range d.Fields() {
		switch key {
		case 1:
			k.Found = d.Bool()
		case 2:
			k.Answer = d.Str()
		}
	}
	return d.End()
}

// JobsEntry is a job as its queue holds it: get's answer, a scan's item and
// a watch's.
type JobsEntry struct {
	Found   bool
	Key     string
	Value   string
	At      int64
	Attempt uint64
	// State is 1 waiting, 2 running, 3 failed, 4 done or 5 cancelled.
	State uint64
	// Err is the job's last failure.
	Err string
	// Repeat is a repeating job's cron text and zone.
	Repeat string
	// Ahead counts the jobs that run before a waiting one, up to 10,000.
	Ahead uint64
	// Progress is what a running job's handler last reported, as JSON.
	Progress string
	// Ran is when the last run a handler finished began, unix milliseconds,
	// and Took how many milliseconds it took.
	Ran  int64
	Took uint64
}

func (e JobsEntry) Append(dst []byte) []byte {
	m := BeginMap(dst)
	if e.Found {
		m.Bool(1, true)
	}
	optionalStr(&m, 2, e.Key)
	optionalStr(&m, 3, e.Value)
	optionalInt(&m, 4, e.At)
	optionalUint(&m, 5, e.Attempt)
	optionalUint(&m, 6, e.State)
	optionalStr(&m, 7, e.Err)
	optionalStr(&m, 8, e.Repeat)
	optionalUint(&m, 9, e.Ahead)
	optionalStr(&m, 10, e.Progress)
	optionalInt(&m, 11, e.Ran)
	optionalUint(&m, 12, e.Took)
	return m.End()
}

func (e *JobsEntry) Decode(body []byte) error {
	d := NewDecoder(body)
	for key := range d.Fields() {
		switch key {
		case 1:
			e.Found = d.Bool()
		case 2:
			e.Key = d.Str()
		case 3:
			e.Value = d.Str()
		case 4:
			e.At = d.Int()
		case 5:
			e.Attempt = d.Uint()
		case 6:
			e.State = d.Uint()
		case 7:
			e.Err = d.Str()
		case 8:
			e.Repeat = d.Str()
		case 9:
			e.Ahead = d.Uint()
		case 10:
			e.Progress = d.Str()
		case 11:
			e.Ran = d.Int()
		case 12:
			e.Took = d.Uint()
		}
	}
	return d.End()
}

// JobsLease is jobs.claim's request: the next due job, leased for Lease
// milliseconds, the queue's when zero.
type JobsLease struct {
	Handle uint64
	Lease  int64
}

func (c JobsLease) Append(dst []byte) []byte {
	m := BeginMap(dst)
	m.Uint(1, c.Handle)
	optionalInt(&m, 2, c.Lease)
	return m.End()
}

func (c *JobsLease) Decode(body []byte) error {
	d := NewDecoder(body)
	for key := range d.Fields() {
		switch key {
		case 1:
			c.Handle = d.Uint()
		case 2:
			c.Lease = d.Duration()
		}
	}
	return d.End()
}

// JobsHeld is a job in a worker's hands: claim's answer, found false when no
// job was due, and an item of a work stream. Job is the number its
// settlement names.
type JobsHeld struct {
	Found   bool
	Job     uint64
	Key     string
	Value   string
	At      int64
	Attempt uint64
	// Cancelled, on a work stream whose workers take cancels, names a job in
	// the client's hands that Cancel took: its handler should stop, and its
	// outcome settles nothing.
	Cancelled bool
}

func (h JobsHeld) Append(dst []byte) []byte {
	m := BeginMap(dst)
	if h.Found {
		m.Bool(1, true)
	}
	optionalUint(&m, 2, h.Job)
	optionalStr(&m, 3, h.Key)
	optionalStr(&m, 4, h.Value)
	optionalInt(&m, 5, h.At)
	optionalUint(&m, 6, h.Attempt)
	if h.Cancelled {
		m.Bool(7, true)
	}
	return m.End()
}

func (h *JobsHeld) Decode(body []byte) error {
	d := NewDecoder(body)
	for key := range d.Fields() {
		switch key {
		case 1:
			h.Found = d.Bool()
		case 2:
			h.Job = d.Uint()
		case 3:
			h.Key = d.Str()
		case 4:
			h.Value = d.Str()
		case 5:
			h.At = d.Int()
		case 6:
			h.Attempt = d.Uint()
		case 7:
			h.Cancelled = d.Bool()
		}
	}
	return d.End()
}

// How a worker settles a job, a JobsOutcome's How.
const (
	JobAck      = 1 // done
	JobRetry    = 2 // this attempt failed: again after the backoff, or At or After
	JobFail     = 3 // failed for good
	JobSnooze   = 4 // again at At or After, the attempt not counted
	JobExtend   = 5 // the lease runs After from now
	JobProgress = 6 // what the handler reports of the job, which stays in its hands
)

// JobsOutcome settles a job a worker held: a settlement's item, and what a
// worker sends back on a work stream.
type JobsOutcome struct {
	Job   uint64
	How   uint64
	Err   string
	At    int64 // unix milliseconds
	After int64 // milliseconds
	// HasAfter says the outcome carries After, 0 included: a retry after
	// nothing runs now, where one without a time waits its backoff.
	HasAfter bool
	// Progress is a progress outcome's report, as JSON.
	Progress string
}

func (o JobsOutcome) Append(dst []byte) []byte {
	m := BeginMap(dst)
	o.appendFields(&m)
	return m.End()
}

func (o JobsOutcome) appendFields(m *Map) {
	m.Uint(1, o.Job)
	m.Uint(2, o.How)
	optionalStr(m, 3, o.Err)
	optionalInt(m, 4, o.At)
	if o.After != 0 || o.HasAfter {
		m.Int(5, o.After)
	}
	optionalStr(m, 6, o.Progress)
}

func (o *JobsOutcome) Decode(body []byte) error {
	d := NewDecoder(body)
	o.decode(&d)
	return d.End()
}

func (o *JobsOutcome) decode(d *Decoder) {
	for key := range d.Fields() {
		switch key {
		case 1:
			o.Job = d.Uint()
		case 2:
			o.How = d.Uint()
		case 3:
			o.Err = d.Str()
		case 4:
			o.At = d.Int()
		case 5:
			o.After, o.HasAfter = d.Duration(), true
		case 6:
			o.Progress = d.Str()
		}
	}
}

// JobsOutcomes is jobs.settle's request: outcomes of claimed jobs, written
// together.
type JobsOutcomes struct {
	Outcomes []JobsOutcome
}

func (s JobsOutcomes) Append(dst []byte) []byte {
	m := BeginMap(dst)
	m.Key(1)
	buf := AppendArray(m.Buf(), len(s.Outcomes))
	for _, outcome := range s.Outcomes {
		buf = outcome.Append(buf)
	}
	m.SetBuf(buf)
	return m.End()
}

func (s *JobsOutcomes) Decode(body []byte) error {
	d := NewDecoder(body)
	for key := range d.Fields() {
		if key != 1 {
			continue
		}
		for range d.Items() {
			var outcome JobsOutcome
			outcome.decode(&d)
			s.Outcomes = append(s.Outcomes, outcome)
		}
	}
	return d.End()
}

// JobsSettled answers a settlement: nil for each outcome written, or the
// error that refused it, in their order.
type JobsSettled struct {
	Errors []*Error
}

func (s JobsSettled) Append(dst []byte) []byte {
	m := BeginMap(dst)
	m.Key(1)
	buf := AppendArray(m.Buf(), len(s.Errors))
	for _, failure := range s.Errors {
		if failure == nil {
			buf = AppendNil(buf)
			continue
		}
		buf = failure.Append(buf)
	}
	m.SetBuf(buf)
	return m.End()
}

func (s *JobsSettled) Decode(body []byte) error {
	d := NewDecoder(body)
	for key := range d.Fields() {
		if key != 1 {
			continue
		}
		for range d.Items() {
			if d.Nil() {
				s.Errors = append(s.Errors, nil)
				continue
			}
			failure := &Error{}
			failure.decode(&d)
			s.Errors = append(s.Errors, failure)
		}
	}
	return d.End()
}

// JobsQuery is jobs.scan's request.
type JobsQuery struct {
	Handle uint64
	// Prefix selects the keys under it, in the byte order of their text.
	Prefix string
	// State 3 with no prefix selects the failed jobs, the last failed first.
	State uint64
	// After is where the page before ended.
	After string
	Limit uint64
}

func (s JobsQuery) Append(dst []byte) []byte {
	m := BeginMap(dst)
	m.Uint(1, s.Handle)
	optionalStr(&m, 2, s.Prefix)
	optionalUint(&m, 3, s.State)
	optionalStr(&m, 4, s.After)
	optionalUint(&m, 5, s.Limit)
	return m.End()
}

func (s *JobsQuery) Decode(body []byte) error {
	d := NewDecoder(body)
	for key := range d.Fields() {
		switch key {
		case 1:
			s.Handle = d.Uint()
		case 2:
			s.Prefix = d.Str()
		case 3:
			s.State = d.Uint()
		case 4:
			s.After = d.Str()
		case 5:
			s.Limit = d.Uint()
		}
	}
	return d.End()
}

// JobsPage ends a scan: More says the page ended before the jobs did, and
// After is where the next begins.
type JobsPage struct {
	More  bool
	After string
}

func (p JobsPage) Append(dst []byte) []byte {
	m := BeginMap(dst)
	if p.More {
		m.Bool(1, true)
	}
	optionalStr(&m, 2, p.After)
	return m.End()
}

func (p *JobsPage) Decode(body []byte) error {
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

// JobsWorkers is jobs.work's request: the queue's Work loop, whose jobs come to
// the client as DATA and whose outcomes go back as DATA. Timeout is a job's
// time in the client's hands, milliseconds, a minute when zero.
type JobsWorkers struct {
	Handle  uint64
	Workers uint64
	Timeout int64
	// UntilIdle ends the loop once no job is due and none runs, as a test
	// wants it.
	UntilIdle bool
	// Cancels asks for a cancelled held item when Cancel takes a job in the
	// client's hands, which a client that stops its handler wants.
	Cancels bool
}

func (w JobsWorkers) Append(dst []byte) []byte {
	m := BeginMap(dst)
	m.Uint(1, w.Handle)
	optionalUint(&m, 2, w.Workers)
	optionalInt(&m, 3, w.Timeout)
	if w.UntilIdle {
		m.Bool(4, true)
	}
	if w.Cancels {
		m.Bool(5, true)
	}
	return m.End()
}

func (w *JobsWorkers) Decode(body []byte) error {
	d := NewDecoder(body)
	for key := range d.Fields() {
		switch key {
		case 1:
			w.Handle = d.Uint()
		case 2:
			w.Workers = d.Uint()
		case 3:
			w.Timeout = d.Duration()
		case 4:
			w.UntilIdle = d.Bool()
		case 5:
			w.Cancels = d.Bool()
		}
	}
	return d.End()
}
