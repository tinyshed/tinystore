//! The totals the store keeps of its instruments, a slot for each set of
//! labels: a counter's since its run began, a timer's and a histogram's
//! count, sum and buckets the same way, and a gauge's value with the
//! connection that set it.
//!
//! ```text
//! inc() ──► its slot's pending sum, an atomic add ──► the next write takes it into its run's total ──► a sample
//! ```
//!
//! A call changes a number in memory and takes no lock but a timer's. A
//! write, every 15 seconds, at a flush and when the store closes, takes what
//! changed into each run's total and writes every live total at one time.
//! A run begins with a zero at the write before its first call, the last
//! time the series is known to have been zero, so that a reset reads as one
//! even when the new count passed the old; it ends once nothing was sent for
//! an hour, its slot freed unless a handle holds it.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use super::buckets::Bounds;
use super::engine::lock;
use super::labels::{Kept, OVERFLOW};
use super::series::{Kind, Labels};
use crate::{Error, Result};

/// The sets of labels an instrument holds at most; a new one past them
/// counts into its series of `__overflow`.
pub(super) const MOST_SETS: usize = 2000;
/// How long a counter's, a timer's or a histogram's series may go without a
/// call before its run ends and its slot is freed.
const IDLE: i64 = 3_600_000;
/// The owner of a gauge nobody has set.
const NOBODY: u64 = u64::MAX;
/// The owner of what this process sets through its own handles.
pub(super) const LOCAL: u64 = 0;

/// What an instrument is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Form {
    Counter,
    Gauge,
    Timer,
    Histogram,
}

impl Form {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Form::Counter => "counter",
            Form::Gauge => "gauge",
            Form::Timer => "timer",
            Form::Histogram => "histogram",
        }
    }

    /// The names an instrument of this form writes and their kinds: its own,
    /// or a timer's and a histogram's count, sum, longest and buckets.
    pub(super) fn written(self, name: &str, buckets: bool) -> Vec<(String, Kind)> {
        match self {
            Form::Counter => vec![(name.to_owned(), Kind::Counter)],
            Form::Gauge => vec![(name.to_owned(), Kind::Gauge)],
            Form::Timer | Form::Histogram => {
                let mut written = vec![
                    (format!("{name}_count"), Kind::Counter),
                    (format!("{name}_sum"), Kind::Counter),
                    (format!("{name}_max"), Kind::Gauge),
                ];
                written.extend(buckets.then(|| (format!("{name}_bucket"), Kind::Histogram)));
                written
            }
        }
    }

    fn measures(self) -> bool {
        matches!(self, Form::Timer | Form::Histogram)
    }
}

/// A series a slot writes: its labels, their identity and its kind.
#[derive(Clone, Debug)]
pub(super) struct Written {
    pub(super) kept: Kept,
    pub(super) identity: [u8; 32],
    pub(super) kind: Kind,
}

impl Written {
    fn of(kept: Kept, kind: Kind) -> Written {
        Written { identity: kept.identity(), kept, kind }
    }
}

/// One set of labels of an instrument and what calls changed of it.
pub(super) struct Slot {
    /// Its labels beside its instrument's name.
    pub(super) kept: Kept,
    /// The series it writes: a counter's or a gauge's own, a timer's count,
    /// sum, longest and buckets, `+Inf` last.
    pub(super) series: Vec<Written>,
    /// A counter's sum of what calls added since the last write, or a
    /// gauge's value: the bits of a float.
    pending: AtomicU64,
    /// A call since the last write, `add(0)` among them.
    sent: AtomicBool,
    /// The last total a program that keeps its own gave.
    absolute: AtomicU64,
    /// The connection that set a gauge last.
    owner: AtomicU64,
    /// A timer's or a histogram's measurements since the last write.
    measured: Option<Mutex<Measured>>,
    /// Labels it refused: nothing writes it.
    detached: bool,
    /// A refused call said.
    said: AtomicBool,
    /// What the writes keep of it.
    pub(super) run: Mutex<Run>,
}

/// What a timer or a histogram measured since the last write.
#[derive(Debug, Default)]
struct Measured {
    count: u64,
    sum: f64,
    longest: f64,
    any: bool,
    /// Each bucket's own count, not yet summed into `le`'s.
    buckets: Vec<u64>,
}

impl Measured {
    /// What it measured, leaving it at zero with as many buckets, so that a
    /// call meanwhile always finds its bucket.
    fn take(&mut self) -> Measured {
        let buckets = vec![0; self.buckets.len()];
        let taken = Measured { buckets: std::mem::replace(&mut self.buckets, buckets), ..*self };
        (self.count, self.sum, self.longest, self.any) = (0, 0.0, 0.0, false);
        taken
    }
}

/// A slot's run, which the writes alone change.
#[derive(Debug, Default)]
pub(super) struct Run {
    live: bool,
    /// The zero the run begins with, until a write takes it.
    zero: Option<i64>,
    total: f64,
    count: u64,
    sum: f64,
    buckets: Vec<u64>,
    /// The time of the last write that found a call.
    active_at: i64,
    /// What a write last refused it for, said once.
    refusal: Option<String>,
}

impl Slot {
    fn new(kept: Kept, series: Vec<Written>, form: Form, buckets: usize) -> Slot {
        let measured =
            form.measures().then(|| Mutex::new(Measured { buckets: vec![0; buckets], ..Measured::default() }));
        Slot {
            kept,
            series,
            pending: AtomicU64::new(0.0_f64.to_bits()),
            sent: AtomicBool::new(false),
            absolute: AtomicU64::new(0),
            owner: AtomicU64::new(NOBODY),
            measured,
            detached: false,
            said: AtomicBool::new(false),
            run: Mutex::default(),
        }
    }

    /// A slot no write takes, for labels a call gave that were refused.
    fn detached(kept: Kept) -> Slot {
        Slot { detached: true, ..Slot::new(kept, Vec::new(), Form::Counter, 0) }
    }

    fn is_overflow(&self) -> bool {
        self.kept.pairs().iter().any(|(label, _)| label == OVERFLOW)
    }

    /// Adds to a counter: false when its pending sum would not be a finite
    /// number, and the call then changes nothing.
    pub(super) fn add(&self, n: f64) -> bool {
        let added = add_float(&self.pending, n);
        self.sent.store(true, Ordering::Release);
        added
    }

    /// What a total the program keeps itself grew since the last it gave,
    /// or all of it when it is smaller, a restart's.
    pub(super) fn advance_to(&self, total: u64) -> u64 {
        let last = self.absolute.swap(total, Ordering::AcqRel);
        if total >= last { total - last } else { total }
    }

    /// Sets a gauge, for `owner`.
    pub(super) fn set(&self, value: f64, owner: u64) {
        self.pending.store(value.to_bits(), Ordering::Release);
        self.owner.store(owner, Ordering::Release);
    }

    /// Moves a gauge by `delta`, for `owner`: false when its value would not
    /// be a finite number.
    pub(super) fn shift(&self, delta: f64, owner: u64) -> bool {
        let moved = add_float(&self.pending, delta);
        if moved {
            self.owner.store(owner, Ordering::Release);
        }
        moved
    }

    /// Counts a timer's duration or a histogram's value into its bucket.
    pub(super) fn measure(&self, value: f64, bounds: Option<&Bounds>) {
        let Some(measured) = &self.measured else { return };
        let mut measured = lock(measured);
        measured.count += 1;
        measured.sum += value;
        measured.longest = if measured.any { measured.longest.max(value) } else { value };
        measured.any = true;
        if let Some(bucket) = bounds.map(|bounds| bounds.bucket_of(value)) {
            measured.buckets[bucket] += 1;
        }
    }

    /// Says once that a call was refused, and why.
    pub(super) fn refuse(&self, why: &str) {
        if !self.said.swap(true, Ordering::AcqRel) {
            tracing::warn!(target: "tinystore", series = %self.kept, "metrics: {why}; said once");
        }
    }
}

/// An instrument: its name, its form, its buckets, and its slots by their
/// labels.
pub(super) struct Instrument {
    pub(super) name: String,
    pub(super) form: Form,
    pub(super) bounds: Option<Bounds>,
    sets: Mutex<Sets>,
    said_overflow: AtomicBool,
    said_labels: AtomicBool,
}

/// An instrument's slots: its own name's without labels, which takes none
/// of the places of its sets, those of its sets of labels, and the one of
/// `__overflow`.
#[derive(Default)]
struct Sets {
    base: Option<Arc<Slot>>,
    by_labels: HashMap<Kept, Arc<Slot>>,
    overflow: Option<Arc<Slot>>,
}

impl Instrument {
    pub(super) fn new(name: &str, form: Form, bounds: Option<Bounds>) -> Instrument {
        Instrument {
            name: name.to_owned(),
            form,
            bounds,
            sets: Mutex::default(),
            said_overflow: AtomicBool::new(false),
            said_labels: AtomicBool::new(false),
        }
    }

    /// The slot of the instrument's own name without labels.
    pub(super) fn base(&self) -> Result<Arc<Slot>> {
        self.slot(&Labels::new())
    }

    /// The slot of `parent`'s labels and `more`: a label given again takes
    /// its new value. Labels it refuses give a slot nothing writes, said
    /// once; past its sets of labels, its series of `__overflow`.
    pub(super) fn with(&self, parent: &Arc<Slot>, more: &[(&str, &str)]) -> Arc<Slot> {
        if parent.detached || parent.is_overflow() {
            return Arc::clone(parent);
        }
        let mut labels = parent.kept.labels();
        labels.extend(more.iter().map(|(label, value)| ((*label).to_owned(), (*value).to_owned())));
        self.slot(&labels).unwrap_or_else(|error| {
            if !self.said_labels.swap(true, Ordering::AcqRel) {
                let name = &self.name;
                let why = "labels refused, written nowhere";
                tracing::warn!(target: "tinystore", %error, "metrics {name}: {why}; said once");
            }
            Arc::new(Slot::detached(parent.kept.clone()))
        })
    }

    fn slot(&self, labels: &Labels) -> Result<Arc<Slot>> {
        if self.form.measures() && labels.contains_key("le") {
            return Err(Error::invalid("label \"le\": a timer's `_bucket` series give it"));
        }
        let kept = Kept::of(&self.name, labels)?;
        let mut sets = lock(&self.sets);
        if labels.is_empty() {
            if let Some(base) = &sets.base {
                return Ok(Arc::clone(base));
            }
            let base = Arc::new(self.made(kept)?);
            sets.base = Some(Arc::clone(&base));
            return Ok(base);
        }
        if let Some(slot) = sets.by_labels.get(&kept) {
            return Ok(Arc::clone(slot));
        }
        if sets.by_labels.len() >= MOST_SETS {
            return self.overflow(&mut sets);
        }
        let slot = Arc::new(self.made(kept.clone())?);
        sets.by_labels.insert(kept, Arc::clone(&slot));
        Ok(slot)
    }

    /// Its series of `__overflow`, said once when it is first counted into.
    fn overflow(&self, sets: &mut Sets) -> Result<Arc<Slot>> {
        if let Some(slot) = &sets.overflow {
            return Ok(Arc::clone(slot));
        }
        if !self.said_overflow.swap(true, Ordering::AcqRel) {
            let name = &self.name;
            let why = format!("past {MOST_SETS} sets of labels, counting into __overflow");
            tracing::warn!(target: "tinystore", "metrics {name}: {why}; said once");
        }
        let slot = Arc::new(self.made(Kept::overflow(&self.name))?);
        sets.overflow = Some(Arc::clone(&slot));
        Ok(slot)
    }

    /// A slot of `kept` and the series it writes.
    fn made(&self, kept: Kept) -> Result<Slot> {
        let buckets = self.bounds.as_ref().map_or(0, Bounds::buckets);
        let mut series = Vec::new();
        for (name, kind) in self.form.written(&self.name, self.bounds.is_some()) {
            if kind == Kind::Histogram {
                let labels = self.bounds.as_ref().map(Bounds::labels).unwrap_or_default();
                for le in labels {
                    series.push(Written::of(kept.derived(&name, Some(("le", &le)))?, kind));
                }
            } else if name == self.name {
                series.push(Written::of(kept.clone(), kind));
            } else {
                series.push(Written::of(kept.derived(&name, None)?, kind));
            }
        }
        Ok(Slot::new(kept, series, self.form, buckets))
    }

    /// Every slot it holds.
    fn slots(&self) -> Vec<Arc<Slot>> {
        let sets = lock(&self.sets);
        sets.base.iter().chain(sets.by_labels.values()).chain(sets.overflow.iter()).cloned().collect()
    }

    /// Frees the slots of runs that ended which no handle holds. Under the
    /// lock no call can take another handle, so a count of the slot's own
    /// references is exact: the set's entry, `ended`'s and the write's plan.
    fn free(&self, ended: &[Arc<Slot>]) {
        const OURS: usize = 3;
        let mut sets = lock(&self.sets);
        for slot in ended {
            if Arc::strong_count(slot) == OURS
                && sets.by_labels.get(&slot.kept).is_some_and(|held| Arc::ptr_eq(held, slot))
            {
                sets.by_labels.remove(&slot.kept);
            }
        }
    }
}

/// A gauge's function, its error as text.
pub(super) type Read = Box<dyn Fn() -> std::result::Result<f64, String> + Send + Sync>;

/// A gauge whose value a function reads at each write.
pub(super) struct Reader {
    pub(super) slot: Arc<Slot>,
    read: Read,
    /// The error said, while it repeats.
    said: Mutex<Option<String>>,
}

impl Reader {
    /// Reads the gauge's value into its slot; a failure, a panic or a value
    /// that is not a finite number leaves it unset this time, said once
    /// while it repeats.
    fn read(&self) {
        let read = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (self.read)()));
        let value = match read {
            Ok(Ok(value)) if value.is_finite() => Ok(value),
            Ok(Ok(value)) => Err(format!("its function gave {value}, where a gauge is a finite number")),
            Ok(Err(error)) => Err(format!("its function failed: {error}")),
            Err(_) => Err("its function panicked".to_owned()),
        };
        let mut said = lock(&self.said);
        match value {
            Ok(value) => {
                self.slot.set(value, LOCAL);
                *said = None;
            }
            Err(why) => {
                self.slot.owner.store(NOBODY, Ordering::Release);
                if said.as_deref() != Some(&why) {
                    let series = &self.slot.kept;
                    tracing::warn!(target: "tinystore", %series, "metrics: {why}, nothing written; said once");
                    *said = Some(why);
                }
            }
        }
    }
}

/// What a name says of itself at the next write: its unit, its help, a
/// timer's bounds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Description {
    pub(super) unit: Option<String>,
    pub(super) help: Option<String>,
    pub(super) buckets: Option<String>,
}

/// One slot's samples of a write: each of its series' value, and the zero a
/// run begins with.
pub(super) struct Planned {
    pub(super) slot: Arc<Slot>,
    pub(super) values: Vec<Value>,
    /// The longest duration a write took from a timer, which a failed write
    /// gives back.
    longest: Option<f64>,
}

/// A series' sample of a write.
#[derive(Clone, Copy, Debug)]
pub(super) struct Value {
    /// The series among its slot's.
    pub(super) series: usize,
    /// The zero its run begins with, at this time.
    pub(super) zero: Option<i64>,
    pub(super) value: f64,
}

/// What a write did with a slot's samples.
pub(super) enum Outcome {
    Written,
    Refused(Error),
    /// Its write failed whole: tried again at the next.
    Failed,
}

/// The instruments of an engine and the totals of their series.
pub(super) struct Totals {
    registry: Mutex<Registry>,
    described: Mutex<BTreeMap<String, Description>>,
    /// The time of the last write, or the engine's start: a run that begins
    /// after it begins with a zero there.
    last_write: AtomicI64,
    next_owner: AtomicU64,
}

#[derive(Default)]
struct Registry {
    /// By name, a timer's and a histogram's by its own.
    instruments: HashMap<String, Arc<Instrument>>,
    readers: HashMap<String, Arc<Reader>>,
    /// The names written, each by the name of the one instrument that
    /// writes it.
    writers: HashMap<String, String>,
}

impl Totals {
    pub(super) fn new(started: i64) -> Totals {
        Totals {
            registry: Mutex::default(),
            described: Mutex::default(),
            last_write: AtomicI64::new(started),
            next_owner: AtomicU64::new(LOCAL + 1),
        }
    }

    /// The instrument of `name`, made the first time; another form or
    /// other buckets than it has, or a name another instrument writes, is
    /// refused.
    pub(super) fn instrument(&self, name: &str, form: Form, bounds: Option<Bounds>) -> Result<Arc<Instrument>> {
        let mut registry = lock(&self.registry);
        if let Some(held) = registry.instruments.get(name) {
            if held.form != form {
                let held = held.form.as_str();
                return Err(Error::invalid(format!(
                    "a {}, where it is a {held} while the store is open",
                    form.as_str()
                )));
            }
            if held.bounds != bounds {
                return Err(Error::invalid("buckets other than it has while the store is open"));
            }
            return Ok(Arc::clone(held));
        }
        let written = form.written(name, bounds.is_some());
        registry.claim(name, &written)?;
        let instrument = Arc::new(Instrument::new(name, form, bounds));
        registry.instruments.insert(name.to_owned(), Arc::clone(&instrument));
        Ok(instrument)
    }

    /// A gauge read by `read` at each write.
    pub(super) fn reader(&self, name: &str, read: Read) -> Result<()> {
        let mut registry = lock(&self.registry);
        registry.claim(name, &[(name.to_owned(), Kind::Gauge)])?;
        let kept = Kept::of(name, &Labels::new())?;
        let slot = Arc::new(Slot::new(kept.clone(), vec![Written::of(kept, Kind::Gauge)], Form::Gauge, 0));
        registry.readers.insert(name.to_owned(), Arc::new(Reader { slot, read, said: Mutex::default() }));
        Ok(())
    }

    /// Keeps what a name says of itself for the next write; a later call
    /// replaces what it gives.
    pub(super) fn describe(&self, name: &str, description: Description) {
        let mut described = lock(&self.described);
        let kept = described.entry(name.to_owned()).or_default();
        kept.unit = description.unit.or(kept.unit.take());
        kept.help = description.help.or(kept.help.take());
        kept.buckets = description.buckets.or(kept.buckets.take());
    }

    pub(super) fn take_descriptions(&self) -> BTreeMap<String, Description> {
        std::mem::take(&mut *lock(&self.described))
    }

    /// Gives back what a failed write took, under what was said since.
    pub(super) fn give_back_descriptions(&self, taken: BTreeMap<String, Description>) {
        let mut described = lock(&self.described);
        for (name, description) in taken {
            let newer = described.remove(&name).unwrap_or_default();
            let unit = newer.unit.or(description.unit);
            let help = newer.help.or(description.help);
            let buckets = newer.buckets.or(description.buckets);
            described.insert(name, Description { unit, help, buckets });
        }
    }

    /// A connection of its own for the gauges a client sets, which leave the
    /// totals when it does.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the wire's sessions are connections, from the slice that builds them")
    )]
    pub(super) fn connect(self: &Arc<Totals>) -> Connection {
        Connection { totals: Arc::clone(self), id: self.next_owner.fetch_add(1, Ordering::Relaxed) }
    }

    /// What the write at `now` writes of every slot: each live run's
    /// totals, a gauge's value, and the zero a run begins with. Runs a call
    /// began begin; those nothing was sent to for an hour end after it.
    ///
    /// A series no call reached before now is zero at now, written or not,
    /// so a run the next call begins begins with its zero here.
    pub(super) fn collect(&self, now: i64) -> Vec<Planned> {
        let (instruments, readers) = {
            let registry = lock(&self.registry);
            let instruments: Vec<Arc<Instrument>> = registry.instruments.values().cloned().collect();
            (instruments, registry.readers.values().cloned().collect::<Vec<_>>())
        };
        let zero = self.last_write.swap(now, Ordering::AcqRel);
        let mut planned = Vec::new();
        for reader in &readers {
            reader.read();
            planned.extend(gauge(&reader.slot));
        }
        for instrument in &instruments {
            let mut ended = Vec::new();
            for slot in instrument.slots() {
                let plan = match instrument.form {
                    Form::Gauge => gauge(&slot),
                    Form::Counter => counted(&slot, now, zero, &mut ended),
                    Form::Timer | Form::Histogram => measured(&slot, now, zero, &mut ended),
                };
                planned.extend(plan);
            }
            instrument.free(&ended);
        }
        planned
    }

    /// Settles a write: each slot's zero taken, or what refused it said
    /// once, or what a failed write took given back.
    pub(super) fn settle(&self, planned: Vec<Planned>, outcomes: Vec<Outcome>) {
        for (plan, outcome) in planned.into_iter().zip(outcomes) {
            let mut run = lock(&plan.slot.run);
            match outcome {
                Outcome::Written => {
                    run.zero = None;
                    run.refusal = None;
                }
                Outcome::Refused(error) => {
                    let why = error.to_string();
                    if run.refusal.as_ref() != Some(&why) {
                        let series = &plan.slot.kept;
                        tracing::warn!(target: "tinystore", %series, "metrics: a write refused it: {why}; said once");
                        run.refusal = Some(why);
                    }
                }
                Outcome::Failed => {
                    drop(run);
                    give_back(&plan);
                }
            }
        }
    }

    /// How many sets of labels the instrument of `name` holds.
    #[cfg(test)]
    pub(super) fn sets_held(&self, name: &str) -> usize {
        lock(&self.registry).instruments.get(name).map_or(0, |instrument| lock(&instrument.sets).by_labels.len())
    }

    /// Unsets the gauges `owner` set last: they are written no longer.
    fn leave(&self, owner: u64) {
        let instruments: Vec<Arc<Instrument>> = lock(&self.registry).instruments.values().cloned().collect();
        for instrument in instruments.iter().filter(|instrument| instrument.form == Form::Gauge) {
            for slot in instrument.slots() {
                let _ = slot.owner.compare_exchange(owner, NOBODY, Ordering::AcqRel, Ordering::Acquire);
            }
        }
    }
}

impl Registry {
    /// Claims the names an instrument writes, each of which one instrument
    /// alone may write.
    fn claim(&mut self, name: &str, written: &[(String, Kind)]) -> Result<()> {
        if self.readers.contains_key(name) {
            return Err(Error::invalid("a gauge its function already reads"));
        }
        if let Some((series, writer)) =
            written.iter().find_map(|(series, _)| Some(series).zip(self.writers.get(series)))
        {
            return Err(Error::invalid(format!(
                "{series} is written by the instrument {writer}: a series has one writer"
            )));
        }
        for (series, _) in written {
            self.writers.insert(series.clone(), name.to_owned());
        }
        Ok(())
    }
}

/// A gauge's value, once a connection set it.
fn gauge(slot: &Arc<Slot>) -> Option<Planned> {
    if slot.owner.load(Ordering::Acquire) == NOBODY {
        return None;
    }
    let value = f64::from_bits(slot.pending.load(Ordering::Acquire));
    Some(Planned { slot: Arc::clone(slot), values: vec![Value { series: 0, zero: None, value }], longest: None })
}

/// A counter's run total: what calls added since the write before taken in,
/// a run begun by the first call after a write that found none.
fn counted(slot: &Arc<Slot>, now: i64, zero: i64, ended: &mut Vec<Arc<Slot>>) -> Option<Planned> {
    let added = f64::from_bits(slot.pending.swap(0.0_f64.to_bits(), Ordering::AcqRel));
    let sent = slot.sent.swap(false, Ordering::AcqRel) || added != 0.0;
    let mut run = lock(&slot.run);
    begin(&mut run, sent, now, zero);
    if !run.live {
        return None;
    }
    let total = run.total + added;
    if total.is_finite() {
        run.total = total;
    } else {
        slot.refuse("a counter's total past what a float holds");
    }
    let values = vec![Value { series: 0, zero: run.zero, value: run.total }];
    end_if_idle(slot, &mut run, now, ended);
    Some(Planned { slot: Arc::clone(slot), values, longest: None })
}

/// A timer's or a histogram's run totals, and the longest since the write
/// before when it measured any.
fn measured(slot: &Arc<Slot>, now: i64, zero: i64, ended: &mut Vec<Arc<Slot>>) -> Option<Planned> {
    let taken = slot.measured.as_ref().map(|measured| lock(measured).take())?;
    let mut run = lock(&slot.run);
    begin(&mut run, taken.any, now, zero);
    if !run.live {
        return None;
    }
    run.count += taken.count;
    run.sum += taken.sum;
    let buckets = taken.buckets.len().max(run.buckets.len());
    run.buckets.resize(buckets, 0);
    for (total, more) in run.buckets.iter_mut().zip(&taken.buckets) {
        *total += more;
    }
    let zero = run.zero;
    let mut values =
        vec![Value { series: 0, zero, value: run.count as f64 }, Value { series: 1, zero, value: run.sum }];
    values.extend(taken.any.then_some(Value { series: 2, zero: None, value: taken.longest }));
    let mut cumulative = 0;
    for (bucket, count) in run.buckets.iter().enumerate() {
        cumulative += count;
        values.push(Value { series: 3 + bucket, zero, value: cumulative as f64 });
    }
    end_if_idle(slot, &mut run, now, ended);
    Some(Planned { slot: Arc::clone(slot), values, longest: taken.any.then_some(taken.longest) })
}

/// Begins a run at a call when none is live: its totals at zero, and its
/// zero written at the last write, when the series was known to be zero.
fn begin(run: &mut Run, sent: bool, now: i64, zero: i64) {
    if !sent {
        return;
    }
    run.active_at = now;
    if !run.live {
        *run = Run { live: true, zero: Some(zero), active_at: now, refusal: run.refusal.take(), ..Run::default() };
    }
}

/// Ends a run nothing was sent to for an hour, after this write; its slot is
/// freed unless a handle holds it.
fn end_if_idle(slot: &Arc<Slot>, run: &mut Run, now: i64, ended: &mut Vec<Arc<Slot>>) {
    if now.saturating_sub(run.active_at) >= IDLE {
        run.live = false;
        run.zero = None;
        ended.push(Arc::clone(slot));
    }
}

/// Adds `delta` to the float whose bits `cell` holds, unless the sum would
/// not be a finite number: a compare and swap, so that no call waits.
fn add_float(cell: &AtomicU64, delta: f64) -> bool {
    let mut bits = cell.load(Ordering::Acquire);
    loop {
        let sum = f64::from_bits(bits) + delta;
        if !sum.is_finite() {
            return false;
        }
        match cell.compare_exchange_weak(bits, sum.to_bits(), Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return true,
            Err(now) => bits = now,
        }
    }
}

/// Gives a timer's longest back to its slot after a failed write.
fn give_back(plan: &Planned) {
    let (Some(longest), Some(measured)) = (plan.longest, &plan.slot.measured) else { return };
    let mut measured = lock(measured);
    measured.longest = if measured.any { measured.longest.max(longest) } else { longest };
    measured.any = true;
}

/// A client's own gauges: they leave the totals when it does.
pub(super) struct Connection {
    totals: Arc<Totals>,
    pub(super) id: u64,
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.totals.leave(self.id);
    }
}
