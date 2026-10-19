//! Every key held or waited for in a store, its holders and its line, as one
//! state behind one lock. Nothing here waits or calls out: what a change owes
//! the holders, their answers and the news that a lease ended, is gathered in
//! [`Effects`] and run once the lock is let go, since a holder's answer may
//! ask the locks again.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use super::lease::{Answer, Ask, Ending, Fence, Granted, LeaseId, Place, Reply, RequestId, Watch};
use crate::encryption::random;
use crate::{Error, Result};

/// Waiters a key's line takes; the next is refused `limit`.
pub(super) const MOST_WAITERS: usize = 10_000;

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
        Ok(Holder { granted: Granted { lease, fence, expires, revokes, watch }, request: ask.request })
    }
}

pub(super) use crate::clock::millis;

/// What a change owes the holders, run outside the lock.
#[derive(Default)]
#[must_use = "effects are run once the lock is let go"]
pub(super) struct Effects {
    answers: Vec<(Reply, Result<Answer>)>,
    endings: Vec<(Arc<Watch>, Ending)>,
    /// Something came due sooner than the timer sleeps to.
    pub(super) sooner: bool,
}

impl Effects {
    fn answer(&mut self, reply: Reply, answer: Result<Answer>) {
        self.answers.push((reply, answer));
    }

    /// How many leases the change ended before their holder let them go.
    pub(super) fn lost(&self) -> usize {
        self.endings.iter().filter(|(_, ending)| matches!(ending, Ending::Lost)).count()
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
}

struct Waiter {
    id: u64,
    ask: Ask,
    gives_up_at: i64,
    reply: Reply,
}

/// A key's holders, and its line, first come first served.
#[derive(Default)]
struct Line {
    holders: Vec<Holder>,
    waiters: VecDeque<Waiter>,
}

impl Line {
    fn is_empty(&self) -> bool {
        self.holders.is_empty() && self.waiters.is_empty()
    }

    /// The earliest moment something of this key comes due: a lease revoked,
    /// a waiter giving up, or a lease running out while someone waits behind
    /// it. One nobody waits behind is found out when someone asks.
    fn next_due(&self) -> Option<i64> {
        let revoked = self.holders.iter().filter_map(|holder| holder.granted.revokes);
        let run_out = self.holders.iter().filter_map(|holder| holder.granted.expires);
        let run_out = run_out.filter(|_| !self.waiters.is_empty());
        let given_up = self.waiters.iter().map(|waiter| waiter.gives_up_at);
        revoked.chain(run_out).chain(given_up).min()
    }

    /// Takes out the leases that are over at `now`.
    fn ended(&mut self, now: i64) -> Vec<Holder> {
        let over = |holder: &Holder| {
            holder.granted.revokes.is_some_and(|at| at <= now) || holder.granted.expires.is_some_and(|at| at <= now)
        };
        let (ended, kept) = std::mem::take(&mut self.holders).into_iter().partition(over);
        self.holders = kept;
        ended
    }

    fn given_up(&mut self, now: i64) -> Vec<Waiter> {
        let mut gone = Vec::new();
        for waiter in std::mem::take(&mut self.waiters) {
            if waiter.gives_up_at <= now {
                gone.push(waiter);
            } else {
                self.waiters.push_back(waiter);
            }
        }
        gone
    }

    fn has_room(&self, limit: u32) -> bool {
        self.holders.len() < limit as usize
    }
}

/// What the store's locks hold now.
#[derive(Default)]
pub(super) struct State {
    lines: HashMap<Place, Line>,
    leases: HashMap<LeaseId, Place>,
    requests: HashMap<RequestId, Asked>,
    waiting: HashMap<u64, Place>,
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
    pub(super) fn ask(&mut self, ask: Ask, reply: Reply, now: i64, mint: &Mint, effects: &mut Effects) -> Option<u64> {
        if self.closed {
            effects.answer(reply, Err(Error::closed(ask.place.to_string())));
            return None;
        }
        let reply = match self.ask_again(&ask, reply, effects) {
            Ok(reply) => reply,
            Err(waits) => return waits,
        };
        let place = ask.place.clone();
        self.settle(&place, now, mint, effects);
        let line = self.lines.entry(place.clone()).or_default();
        let free = line.waiters.is_empty() && line.has_room(ask.limit);
        let waits = if free {
            self.grant_at_once(ask, reply, now, mint, effects);
            None
        } else {
            self.wait(ask, reply, now, effects)
        };
        self.keep_or_forget(&place, effects);
        waits
    }

    /// Answers a request asked before, by its id: the lease it holds, or its
    /// place in line, which this ask takes over, and whose waiter's id it
    /// gives as `ask` would. Gives the reply back when the request is new.
    fn ask_again(&mut self, ask: &Ask, reply: Reply, effects: &mut Effects) -> Result<Reply, Option<u64>> {
        let Some(asked) = ask.request.and_then(|request| self.requests.get(&request).copied()) else {
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

    fn grant_at_once(&mut self, ask: Ask, reply: Reply, now: i64, mint: &Mint, effects: &mut Effects) {
        match mint.grant(&ask, now) {
            Ok(holder) => {
                effects.answer(reply, Ok(Answer::Granted(holder.granted.clone())));
                self.hold(&ask.place, holder);
            }
            Err(error) => effects.answer(reply, Err(error.within(&ask.place))),
        }
    }

    fn wait(&mut self, ask: Ask, reply: Reply, now: i64, effects: &mut Effects) -> Option<u64> {
        if ask.wait.is_zero() {
            effects.answer(reply, Ok(Answer::Busy));
            return None;
        }
        let place = ask.place.clone();
        let line = self.lines.entry(place.clone()).or_default();
        if line.waiters.len() >= MOST_WAITERS {
            let refused = Error::limit(format!("{MOST_WAITERS} wait for it already")).within(&place);
            effects.answer(reply, Err(refused));
            return None;
        }
        self.next_waiter += 1;
        let id = self.next_waiter;
        if let Some(request) = ask.request {
            self.requests.insert(request, Asked::Waiting(id));
        }
        let gives_up_at = now.saturating_add(millis(ask.wait));
        line.waiters.push_back(Waiter { id, ask, gives_up_at, reply });
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
            effects.endings.push((holder.granted.watch, Ending::Lost));
        }
        for waiter in given_up {
            self.forget_waiter(&waiter);
            effects.answer(waiter.reply, Ok(Answer::Busy));
        }
        self.give_on(place, now, mint, effects);
    }

    /// Gives the key to the waiters at the front of its line while it has
    /// room for them.
    fn give_on(&mut self, place: &Place, now: i64, mint: &Mint, effects: &mut Effects) {
        loop {
            let Some(line) = self.lines.get_mut(place) else { return };
            let Some(waiter) = line.waiters.front().filter(|waiter| line.has_room(waiter.ask.limit)) else { return };
            let granted = mint.grant(&waiter.ask, now);
            let Some(waiter) = line.waiters.pop_front() else { return };
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
        self.lines.entry(place.clone()).or_default().holders.push(holder);
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

    /// Lets go of a lease its holder ended, and gives the key on. Says
    /// whether the lease was still held.
    pub(super) fn release(&mut self, lease: &LeaseId, now: i64, mint: &Mint, effects: &mut Effects) -> bool {
        let Some(place) = self.leases.get(lease).cloned() else { return false };
        let Some(line) = self.lines.get_mut(&place) else { return false };
        let Some(at) = line.holders.iter().position(|holder| &holder.granted.lease == lease) else { return false };
        let holder = line.holders.swap_remove(at);
        self.forget_lease(&holder);
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
    #[cfg(feature = "wire")]
    pub(super) fn cancel(&mut self, id: u64) {
        let Some(place) = self.waiting.get(&id).cloned() else { return };
        let Some(line) = self.lines.get_mut(&place) else { return };
        if let Some(at) = line.waiters.iter().position(|waiter| waiter.id == id)
            && let Some(waiter) = line.waiters.remove(at)
        {
            self.forget_waiter(&waiter);
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
        ttl: std::time::Duration,
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

    #[cfg(feature = "wire")]
    fn holder_mut(&mut self, lease: &LeaseId) -> Option<&mut Holder> {
        let place = self.leases.get(lease)?;
        self.lines.get_mut(place)?.holders.iter_mut().find(|holder| &holder.granted.lease == lease)
    }

    fn waiter_mut(&mut self, id: u64) -> Option<&mut Waiter> {
        let place = self.waiting.get(&id)?;
        self.lines.get_mut(place)?.waiters.iter_mut().find(|waiter| waiter.id == id)
    }

    #[cfg(test)]
    pub(super) fn waiting(&self) -> usize {
        self.waiting.len()
    }

    /// Ends every lease and answers every waiter: the store closed.
    pub(super) fn close(&mut self, effects: &mut Effects) {
        self.closed = true;
        for (place, line) in self.lines.drain() {
            for holder in line.holders {
                effects.endings.push((holder.granted.watch, Ending::Closed));
            }
            for waiter in line.waiters {
                effects.answer(waiter.reply, Err(Error::closed(place.to_string())));
            }
        }
        self.leases.clear();
        self.requests.clear();
        self.waiting.clear();
        self.due.clear();
    }
}
