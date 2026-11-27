//! Every key held or waited for in a store, its holders and its line, as one
//! state behind one lock. Nothing here waits or calls out: what a change owes
//! the holders, their answers and the news that a lease ended, is gathered in
//! [`Effects`] and run once the lock is let go, since a holder's answer may
//! ask the locks again.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use super::breaker::{Admission, Breaker, KeyState, Outcome, Rule};
use super::lease::{Answer, Ask, Ending, Fence, Granted, LeaseId, Place, Priority, Reply, RequestId, Watch};
use crate::clock::from_unix_millis;
use crate::encryption::random;
use crate::limits::LOCK_WAITERS;
use crate::{Error, Result};

/// Waiters a key's line takes; the next is refused `limit`.
pub(super) const MOST_WAITERS: usize = LOCK_WAITERS.bound as usize;

/// What gives leases: the store's epoch, written each time its locks open,
/// and the count of leases since.
pub(super) struct Mint {
    pub(super) epoch: u64,
    pub(super) count: AtomicU64,
}

impl Mint {
    fn grant(&self, ask: &Ask, now: i64) -> Result<Holder> {
        let lease = random::<16>()?;
        let fence = Fence::of(self.epoch, self.count.fetch_add(1, Ordering::Relaxed) + 1);
        let expires = ask.ttl.map(|ttl| now.saturating_add(millis(ttl)));
        let watch = Arc::new(Watch::default());
        let revokes = ask.max_hold.map(|bound| now.saturating_add(millis(bound)));
        let granted = Granted { lease, fence, expires, revokes, watch };
        let background = ask.priority == Priority::Background;
        Ok(Holder { granted, request: ask.request, background, breaker: ask.breaker, probe: ask.probe })
    }
}

pub(super) use crate::clock::millis;

/// What a change owes the holders, run outside the lock.
#[derive(Default)]
#[must_use = "effects are run once the lock is let go"]
pub(super) struct Effects {
    answers: Vec<(Reply, Result<Answer>)>,
    endings: Vec<(Arc<Watch>, Ending)>,
    /// The name of each lease the change ended before its holder let it go.
    lost: Vec<Arc<str>>,
    /// Something came due sooner than the timer sleeps to.
    pub(super) sooner: bool,
}

impl Effects {
    fn answer(&mut self, reply: Reply, answer: Result<Answer>) {
        self.answers.push((reply, answer));
    }

    /// The names of the leases the change ended before their holder let them
    /// go, one a lease.
    pub(super) fn lost(&self) -> &[Arc<str>] {
        &self.lost
    }

    pub(super) fn run(self) {
        for (watch, ending) in self.endings {
            watch.end(ending);
        }
        for (reply, answer) in self.answers {
            reply(answer);
        }
    }
}

pub(super) struct Holder {
    pub(super) granted: Granted,
    request: Option<RequestId>,
    background: bool,
    breaker: Option<Rule>,
    probe: bool,
}

struct Waiter {
    id: u64,
    ask: Ask,
    gives_up_at: i64,
    reply: Reply,
}

/// A key's holders, and its two lines, each first come first served: the
/// urgent asks', given the key first, and the background asks', which take
/// what room the urgent leave, `background` places at most.
///
/// A key no background hold ever waited for keeps its second line empty,
/// and an empty `VecDeque` holds no memory.
#[derive(Default)]
struct Line {
    holders: Vec<Holder>,
    waiters: VecDeque<Waiter>,
    background: VecDeque<Waiter>,
    /// How many of `holders` hold in the background.
    background_held: u32,
}

impl Line {
    fn is_empty(&self) -> bool {
        self.holders.is_empty() && self.waiters.is_empty() && self.background.is_empty()
    }

    fn waiting(&self) -> usize {
        self.waiters.len() + self.background.len()
    }

    fn all_waiters(&self) -> impl Iterator<Item = &Waiter> {
        self.waiters.iter().chain(&self.background)
    }

    fn queue(&mut self, priority: Priority) -> &mut VecDeque<Waiter> {
        match priority {
            Priority::Urgent => &mut self.waiters,
            Priority::Background => &mut self.background,
        }
    }

    /// The earliest moment something of this key comes due: a lease revoked,
    /// a waiter giving up, or a lease running out. One nobody waits behind
    /// is due too, since a client gone never lets it go and its key may never
    /// be asked again.
    fn next_due(&self) -> Option<i64> {
        let revoked = self.holders.iter().filter_map(|holder| holder.granted.revokes);
        let run_out = self.holders.iter().filter_map(|holder| holder.granted.expires);
        let given_up = self.all_waiters().map(|waiter| waiter.gives_up_at);
        revoked.chain(run_out).chain(given_up).min()
    }

    /// Takes out the leases that are over at `now`.
    fn ended(&mut self, now: i64) -> Vec<Holder> {
        let over = |holder: &Holder| {
            holder.granted.revokes.is_some_and(|at| at <= now) || holder.granted.expires.is_some_and(|at| at <= now)
        };
        let (ended, kept): (Vec<Holder>, Vec<Holder>) = std::mem::take(&mut self.holders).into_iter().partition(over);
        self.holders = kept;
        for holder in &ended {
            self.let_go(holder);
        }
        ended
    }

    fn take_holder(&mut self, at: usize) -> Holder {
        let holder = self.holders.swap_remove(at);
        self.let_go(&holder);
        holder
    }

    fn let_go(&mut self, holder: &Holder) {
        if holder.background {
            self.background_held -= 1;
        }
    }

    fn given_up(&mut self, now: i64) -> Vec<Waiter> {
        let mut gone = Vec::new();
        for queue in [&mut self.waiters, &mut self.background] {
            if queue.iter().all(|waiter| waiter.gives_up_at > now) {
                continue;
            }
            for waiter in std::mem::take(queue) {
                if waiter.gives_up_at <= now {
                    gone.push(waiter);
                } else {
                    queue.push_back(waiter);
                }
            }
        }
        gone
    }

    fn has_room(&self, limit: u32) -> bool {
        self.holders.len() < limit as usize
    }

    /// Whether background holds may take one more place: fewer than its
    /// share hold in the background.
    fn has_background_room(&self, share: Option<u32>) -> bool {
        share.is_none_or(|share| self.background_held < share)
    }

    /// Whether `ask` is granted at once: nobody of its priority or before it
    /// waits, and it has room. A background ask waiting never holds an urgent
    /// one back.
    fn admits(&self, ask: &Ask) -> bool {
        let first = match ask.priority {
            Priority::Urgent => self.waiters.is_empty(),
            Priority::Background => {
                self.waiters.is_empty() && self.background.is_empty() && self.has_background_room(ask.background)
            }
        };
        first && self.has_room(ask.limit)
    }

    /// Which line's first waiter is given the key next, if any has room: the
    /// urgent line's first, and the background line's only while no urgent
    /// ask waits.
    fn next_given(&self) -> Option<Priority> {
        if let Some(first) = self.waiters.front() {
            return self.has_room(first.ask.limit).then_some(Priority::Urgent);
        }
        let first = self.background.front()?;
        let room = self.has_room(first.ask.limit) && self.has_background_room(first.ask.background);
        room.then_some(Priority::Background)
    }
}

/// How a name's keys stand: the asks in their lines and the leases held.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Standing {
    pub(super) waiting: usize,
    pub(super) held: usize,
}

/// What the store's locks hold now.
#[derive(Default)]
pub(super) struct State {
    lines: HashMap<Place, Line>,
    leases: HashMap<LeaseId, Place>,
    requests: HashMap<RequestId, Asked>,
    waiting: HashMap<u64, Place>,
    /// The keys whose holds failed since their last success, or that are
    /// down: empty for a store whose semaphores keep no breaker, so that an
    /// ask of one costs a branch.
    breakers: HashMap<Place, Breaker>,
    /// When each key next comes due. An entry goes stale when its key
    /// changes; a stale one costs a look and is dropped.
    due: BinaryHeap<Reverse<(i64, Place)>>,
    next_waiter: u64,
    closed: bool,
}

/// Where a request stands, by its id.
#[derive(Clone, Copy)]
enum Asked {
    Waiting(u64),
    Held(LeaseId),
}

impl State {
    /// Grants the key, puts the ask in line, or answers busy; says the
    /// waiter's id when it waits.
    ///
    /// A key its breaker put down refuses the ask at once, unless its pause
    /// is over and no other ask goes through: this one does.
    pub(super) fn ask(
        &mut self,
        mut ask: Ask,
        reply: Reply,
        now: i64,
        mint: &Mint,
        effects: &mut Effects,
    ) -> Option<u64> {
        if self.closed {
            effects.answer(reply, Err(Error::closed(ask.place.to_string())));
            return None;
        }
        let reply = match self.ask_again(&ask, reply, now, mint, effects) {
            Ok(reply) => reply,
            Err(waits) => return waits,
        };
        let place = ask.place.clone();
        match self.admit(&place, now) {
            Admission::Open => {}
            Admission::Probe => ask.probe = true,
            Admission::Refused(refused) => {
                effects.answer(reply, Err(refused.within(&place)));
                return None;
            }
        }
        let probe = ask.probe;
        self.settle(&place, now, mint, effects);
        let line = self.lines.entry(place.clone()).or_default();
        let (granted, waits) = if line.admits(&ask) {
            (self.grant_at_once(ask, reply, now, mint, effects), None)
        } else {
            (false, self.wait(ask, reply, now, effects))
        };
        if probe && !granted && waits.is_none() {
            self.probe_left(&place);
        }
        self.keep_or_forget(&place, effects);
        waits
    }

    /// What the key's breaker lets an ask do: a store with no breaker kept
    /// looks up nothing.
    fn admit(&mut self, place: &Place, now: i64) -> Admission {
        if self.breakers.is_empty() {
            return Admission::Open;
        }
        self.breakers.get_mut(place).map_or(Admission::Open, |breaker| breaker.admit(now))
    }

    /// The probe of a down key left without its outcome: the next ask goes
    /// through.
    fn probe_left(&mut self, place: &Place) {
        if let Some(breaker) = self.breakers.get_mut(place) {
            breaker.probe_abandoned();
        }
    }

    /// Answers a request asked before, by its id: the lease it holds, or its
    /// place in line, which this ask takes over, and whose waiter's id it
    /// gives as `ask` would. Gives the reply back when the request is new.
    ///
    /// The holder counts its lease's deadline from this answer, so the lease
    /// is settled first. One that ran out ends, and the request is new, or its
    /// holder would hold the key beside the next one given it. One still held
    /// lasts its `ttl` from `now`, as a renewal would, or the store would let
    /// it go before its holder gives up.
    fn ask_again(
        &mut self,
        ask: &Ask,
        reply: Reply,
        now: i64,
        mint: &Mint,
        effects: &mut Effects,
    ) -> Result<Reply, Option<u64>> {
        let Some(request) = ask.request else { return Ok(reply) };
        if let Some(Asked::Held(lease)) = self.requests.get(&request).copied()
            && let Some(place) = self.leases.get(&lease).cloned()
        {
            self.settle(&place, now, mint, effects);
            self.last_from(&lease, ask.ttl, now);
            self.keep_or_forget(&place, effects);
        }
        let Some(asked) = self.requests.get(&request).copied() else {
            return Ok(reply);
        };
        match asked {
            Asked::Held(lease) => {
                let granted = self.holder(&lease).map(|holder| holder.granted.clone());
                let answer = granted.map(Answer::Granted).ok_or_else(|| Error::internal("a request's lease is gone"));
                effects.answer(reply, answer);
            }
            Asked::Waiting(id) => {
                let waiter = self.waiter_mut(id).map(|waiter| std::mem::replace(&mut waiter.reply, reply));
                if let Some(earlier) = waiter {
                    effects.answer(earlier, Err(Error::new(crate::ErrorKind::Cancelled, "asked again")));
                    return Err(Some(id));
                }
            }
        }
        Err(None)
    }

    /// Makes a lease held through a connection last `ttl` from `now`.
    fn last_from(&mut self, lease: &LeaseId, ttl: Option<Duration>, now: i64) {
        if let Some(holder) = self.holder_mut(lease)
            && holder.granted.expires.is_some()
            && let Some(ttl) = ttl
        {
            holder.granted.expires = Some(now.saturating_add(millis(ttl)));
        }
    }

    /// Grants the key; says whether it was given.
    fn grant_at_once(&mut self, ask: Ask, reply: Reply, now: i64, mint: &Mint, effects: &mut Effects) -> bool {
        match mint.grant(&ask, now) {
            Ok(holder) => {
                effects.answer(reply, Ok(Answer::Granted(holder.granted.clone())));
                self.hold(&ask.place, holder);
                true
            }
            Err(error) => {
                effects.answer(reply, Err(error.within(&ask.place)));
                false
            }
        }
    }

    fn wait(&mut self, ask: Ask, reply: Reply, now: i64, effects: &mut Effects) -> Option<u64> {
        if ask.wait.is_zero() {
            effects.answer(reply, Ok(Answer::Busy));
            return None;
        }
        let place = ask.place.clone();
        let line = self.lines.entry(place.clone()).or_default();
        if line.waiting() >= MOST_WAITERS {
            let waiting = (line.waiting() + 1) as u64;
            let refused = LOCK_WAITERS.refuse(waiting, format!("{MOST_WAITERS} wait for it already")).within(&place);
            effects.answer(reply, Err(refused));
            return None;
        }
        self.next_waiter += 1;
        let id = self.next_waiter;
        if let Some(request) = ask.request {
            self.requests.insert(request, Asked::Waiting(id));
        }
        let gives_up_at = now.saturating_add(millis(ask.wait));
        line.queue(ask.priority).push_back(Waiter { id, ask, gives_up_at, reply });
        self.waiting.insert(id, place);
        Some(id)
    }

    /// Ends the leases of `place` that are over, lets go the waiters whose
    /// wait ended, and gives the key to the first in line while there is room.
    fn settle(&mut self, place: &Place, now: i64, mint: &Mint, effects: &mut Effects) {
        let Some(line) = self.lines.get_mut(place) else { return };
        let ended = line.ended(now);
        let given_up = line.given_up(now);
        for holder in ended {
            self.forget_lease(&holder);
            // revoked at its max_hold or timeout, the work ran past its time; run out, its client went
            let revoked = holder.granted.revokes.is_some_and(|at| at <= now);
            self.tell(place, &holder, if revoked { Outcome::Failed } else { Outcome::Abandoned }, now, effects);
            effects.lost.push(Arc::clone(place.name()));
            effects.endings.push((holder.granted.watch, Ending::Lost));
        }
        for waiter in given_up {
            self.forget_waiter(&waiter);
            if waiter.ask.probe {
                self.probe_left(place);
            }
            effects.answer(waiter.reply, Ok(Answer::Busy));
        }
        self.give_on(place, now, mint, effects);
    }

    /// Gives the key to the waiters at the front of its lines while it has
    /// room for them, the urgent line first.
    fn give_on(&mut self, place: &Place, now: i64, mint: &Mint, effects: &mut Effects) {
        loop {
            let Some(line) = self.lines.get_mut(place) else { return };
            let Some(priority) = line.next_given() else { return };
            let queue = line.queue(priority);
            let Some(waiter) = queue.pop_front() else { return };
            let granted = mint.grant(&waiter.ask, now);
            self.forget_waiter(&waiter);
            match granted {
                Ok(holder) => {
                    effects.answer(waiter.reply, Ok(Answer::Granted(holder.granted.clone())));
                    self.hold(place, holder);
                }
                Err(error) => effects.answer(waiter.reply, Err(error.within(place))),
            }
        }
    }

    fn hold(&mut self, place: &Place, holder: Holder) {
        self.leases.insert(holder.granted.lease, place.clone());
        if let Some(request) = holder.request {
            self.requests.insert(request, Asked::Held(holder.granted.lease));
        }
        let line = self.lines.entry(place.clone()).or_default();
        if holder.background {
            line.background_held += 1;
        }
        line.holders.push(holder);
    }

    fn forget_lease(&mut self, holder: &Holder) {
        self.leases.remove(&holder.granted.lease);
        if let Some(request) = holder.request {
            self.requests.remove(&request);
        }
    }

    fn forget_waiter(&mut self, waiter: &Waiter) {
        self.waiting.remove(&waiter.id);
        if let Some(request) = waiter.ask.request {
            self.requests.remove(&request);
        }
    }

    /// Tells the key's breaker how a hold ended. A probe's success puts the
    /// key up and its failure pauses it again; any other hold's failure
    /// counts by its rule, and the one that puts the key down refuses every
    /// ask in its lines. A success while the key is up forgets its count.
    fn tell(&mut self, place: &Place, holder: &Holder, ended: Outcome, now: i64, effects: &mut Effects) {
        if holder.probe {
            match ended {
                Outcome::Succeeded => drop(self.breakers.remove(place)),
                Outcome::Failed => {
                    if let Some(breaker) = self.breakers.get_mut(place) {
                        breaker.probe_failed(now);
                    }
                }
                Outcome::Abandoned => self.probe_left(place),
            }
            return;
        }
        match (ended, holder.breaker) {
            (Outcome::Failed, Some(rule)) => {
                let breaker = self.breakers.entry(place.clone()).or_insert_with(|| Breaker::new(rule));
                if breaker.failed(rule, now) {
                    self.refuse_waiters(place, now, effects);
                }
            }
            // the emptiness first, so that a store with no breaker looks up nothing
            (Outcome::Succeeded, _)
                if !self.breakers.is_empty() && self.breakers.get(place).is_some_and(|breaker| !breaker.down()) =>
            {
                self.breakers.remove(place);
            }
            _ => {}
        }
    }

    /// Answers every ask in the key's lines as its breaker refuses a new one.
    fn refuse_waiters(&mut self, place: &Place, now: i64, effects: &mut Effects) {
        let Some(breaker) = self.breakers.get(place) else { return };
        let Some(since) = breaker.down_since() else { return };
        let refusal = breaker.refusal(since, now).within(place);
        let Some(line) = self.lines.get_mut(place) else { return };
        let refused: Vec<Waiter> = line.waiters.drain(..).chain(line.background.drain(..)).collect();
        for waiter in refused {
            self.forget_waiter(&waiter);
            effects.answer(waiter.reply, Err(refusal.duplicate()));
        }
    }

    /// How a key stands at `now`: its breaker's word, and its lines'.
    pub(super) fn state_of(&mut self, place: &Place, now: i64, mint: &Mint, effects: &mut Effects) -> KeyState {
        self.settle(place, now, mint, effects);
        self.keep_or_forget(place, effects);
        let line = self.lines.get(place);
        let breaker = self.breakers.get(place);
        let count = |count: usize| u32::try_from(count).unwrap_or(u32::MAX);
        KeyState {
            up: !breaker.is_some_and(Breaker::down),
            down_since: breaker.and_then(Breaker::down_since).map(from_unix_millis),
            last_failure: breaker.and_then(Breaker::last_failure).map(from_unix_millis),
            waiting: count(line.map_or(0, Line::waiting)),
            holding: count(line.map_or(0, |line| line.holders.len())),
        }
    }

    /// Lets go of a lease its holder ended, as it ended, and gives the key
    /// on. Says whether the lease was still held.
    pub(super) fn release(
        &mut self,
        lease: &LeaseId,
        ended: Outcome,
        now: i64,
        mint: &Mint,
        effects: &mut Effects,
    ) -> bool {
        let Some(place) = self.leases.get(lease).cloned() else { return false };
        let Some(line) = self.lines.get_mut(&place) else { return false };
        let Some(at) = line.holders.iter().position(|holder| &holder.granted.lease == lease) else { return false };
        let holder = line.take_holder(at);
        self.forget_lease(&holder);
        self.tell(&place, &holder, ended, now, effects);
        self.settle(&place, now, mint, effects);
        self.keep_or_forget(&place, effects);
        true
    }

    /// Settles every key that came due by `now`.
    pub(super) fn tick(&mut self, now: i64, mint: &Mint, effects: &mut Effects) {
        let mut places = Vec::new();
        while let Some(Reverse((at, _))) = self.due.peek() {
            if *at > now {
                break;
            }
            let Some(Reverse((_, place))) = self.due.pop() else { break };
            places.push(place);
        }
        places.sort();
        places.dedup();
        for place in places {
            self.settle(&place, now, mint, effects);
            self.keep_or_forget(&place, effects);
        }
    }

    /// When the earliest key comes due; none when nothing will.
    pub(super) fn next_due(&self) -> Option<i64> {
        self.due.peek().map(|Reverse((at, _))| *at)
    }

    /// Forgets a key nobody holds or waits for, or keeps its next time.
    fn keep_or_forget(&mut self, place: &Place, effects: &mut Effects) {
        let Some(line) = self.lines.get(place) else { return };
        if line.is_empty() {
            self.lines.remove(place);
            return;
        }
        let Some(at) = line.next_due() else { return };
        if self.next_due().is_none_or(|next| at < next) {
            effects.sooner = true;
        }
        self.due.push(Reverse((at, place.clone())));
        self.compact();
    }

    /// Rebuilds the times due once stale ones outnumber the keys.
    fn compact(&mut self) {
        if self.due.len() <= 2 * self.lines.len() + 64 {
            return;
        }
        let due = self.lines.iter().filter_map(|(place, line)| line.next_due().map(|at| Reverse((at, place.clone()))));
        self.due = due.collect();
    }

    /// Takes a waiter out of its line, its call cancelled or its connection
    /// gone; its reply is dropped unanswered.
    #[cfg(any(feature = "wire", feature = "async"))]
    pub(super) fn cancel(&mut self, id: u64) {
        let Some(place) = self.waiting.get(&id).cloned() else { return };
        let Some(line) = self.lines.get_mut(&place) else { return };
        let removed = [&mut line.waiters, &mut line.background].into_iter().find_map(|queue| {
            let at = queue.iter().position(|waiter| waiter.id == id)?;
            queue.remove(at)
        });
        if let Some(waiter) = removed {
            self.forget_waiter(&waiter);
            if waiter.ask.probe {
                self.probe_left(&place);
            }
        }
        if self.lines.get(&place).is_some_and(Line::is_empty) {
            self.lines.remove(&place);
        }
    }

    /// Renews a lease held through a connection for `ttl` from `now`, and
    /// says its new deadline; one that ran out or was revoked is `lost`.
    #[cfg(feature = "wire")]
    pub(super) fn renew(
        &mut self,
        lease: &LeaseId,
        ttl: Duration,
        now: i64,
        mint: &Mint,
        effects: &mut Effects,
    ) -> Result<i64> {
        let place = self.leases.get(lease).cloned();
        if let Some(place) = &place {
            self.settle(place, now, mint, effects);
        }
        let Some(holder) = self.holder_mut(lease) else {
            return Err(Error::new(crate::ErrorKind::Lost, "no lease of this id is held"));
        };
        let expires = now.saturating_add(millis(ttl));
        holder.granted.expires = Some(expires);
        if let Some(place) = place {
            self.keep_or_forget(&place, effects);
        }
        Ok(expires)
    }

    fn holder(&self, lease: &LeaseId) -> Option<&Holder> {
        let place = self.leases.get(lease)?;
        self.lines.get(place)?.holders.iter().find(|holder| &holder.granted.lease == lease)
    }

    fn holder_mut(&mut self, lease: &LeaseId) -> Option<&mut Holder> {
        let place = self.leases.get(lease)?;
        self.lines.get_mut(place)?.holders.iter_mut().find(|holder| &holder.granted.lease == lease)
    }

    fn waiter_mut(&mut self, id: u64) -> Option<&mut Waiter> {
        let place = self.waiting.get(&id)?;
        let line = self.lines.get_mut(place)?;
        line.waiters.iter_mut().chain(&mut line.background).find(|waiter| waiter.id == id)
    }

    #[cfg(test)]
    pub(super) fn waiting(&self) -> usize {
        self.waiting.len()
    }

    /// How many asks wait in line and how many leases are held, by the name
    /// of their lock or semaphore.
    pub(super) fn by_name(&self) -> HashMap<Arc<str>, Standing> {
        let mut by_name: HashMap<Arc<str>, Standing> = HashMap::new();
        for (place, line) in &self.lines {
            let standing = by_name.entry(Arc::clone(place.name())).or_default();
            standing.waiting += line.waiting();
            standing.held += line.holders.len();
        }
        by_name
    }

    /// The keys, leases, requests and waiters the state keeps.
    #[cfg(all(test, feature = "wire"))]
    pub(super) fn kept(&self) -> usize {
        self.lines.len() + self.leases.len() + self.requests.len() + self.waiting.len() + self.breakers.len()
    }

    /// Ends every lease and answers every waiter: the store closed.
    pub(super) fn close(&mut self, effects: &mut Effects) {
        self.closed = true;
        for (place, line) in self.lines.drain() {
            for holder in line.holders {
                effects.endings.push((holder.granted.watch, Ending::Closed));
            }
            for waiter in line.waiters.into_iter().chain(line.background) {
                effects.answer(waiter.reply, Err(Error::closed(place.to_string())));
            }
        }
        self.leases.clear();
        self.requests.clear();
        self.waiting.clear();
        self.breakers.clear();
        self.due.clear();
    }
}
