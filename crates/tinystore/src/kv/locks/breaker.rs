//! A semaphore's breaker: a key whose holds failed `failures` times in a row
//! is down for `pause`, and every ask of it, waiting or new, is refused at
//! once rather than wait for a service that will not answer.
//!
//! | the key is         | an ask                   | then                                       |
//! |--------------------|--------------------------|--------------------------------------------|
//! | up                 | waits or holds as ever   | a failure counts, a success resets         |
//! | down, in its pause | refused `unavailable`    |                                            |
//! | down, pause over   | the first is let through | its success: up; its failure: paused again |
//! | down, one through  | refused `unavailable`    | its holder gone: the next ask goes through |
//!
//! It is kept beside the lines, in the memory of the process that holds the
//! store, so that every client of the store hears one answer; a restart of
//! that process starts every key up, as it starts every line empty. The end
//! of a pause needs no timer: nothing changes then until the next ask, which
//! reads the time.

use std::time::Duration;

use crate::clock::{millis, utc};
use crate::{Error, ErrorKind};

/// What a semaphore's breaker is set to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Rule {
    /// How many failed holds in a row put the key down, 1 or more.
    pub(crate) failures: u32,
    pub(crate) pause: Duration,
}

/// How a hold ended, as its holder tells the store when it lets go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Succeeded,
    /// The work failed, or the hold ran past its timeout or `max_hold`.
    Failed,
    /// Its holder went before it could say, a client gone or an awaited
    /// hold's future dropped: the service did not fail, the caller did.
    Abandoned,
}

/// A key's breaker, kept only while it has something to say: a failure since
/// the last success, or a pause.
#[derive(Debug)]
pub(super) struct Breaker {
    /// The rule of the last failed hold, which a probe that fails pauses by.
    rule: Rule,
    in_a_row: u32,
    last_failure: Option<i64>,
    /// When the key went down; none while it is up.
    down_since: Option<i64>,
    /// When the pause ends and an ask may go through, unix milliseconds.
    resumes_at: i64,
    /// The failures in a row that put the key down, for the refusal.
    tripped_at: u32,
    /// An ask went through once the pause ended, and has not ended.
    probing: bool,
}

/// What a key's breaker lets an ask do.
pub(super) enum Admission {
    /// Ask as ever.
    Open,
    /// The one ask let through once the pause ended: its outcome says
    /// whether the key is up again.
    Probe,
    Refused(Error),
}

impl Breaker {
    pub(super) fn new(rule: Rule) -> Breaker {
        Breaker {
            rule,
            in_a_row: 0,
            last_failure: None,
            down_since: None,
            resumes_at: 0,
            tripped_at: 0,
            probing: false,
        }
    }

    pub(super) fn admit(&mut self, now: i64) -> Admission {
        let Some(since) = self.down_since else { return Admission::Open };
        if now < self.resumes_at || self.probing {
            return Admission::Refused(self.refusal(since, now));
        }
        self.probing = true;
        Admission::Probe
    }

    /// Counts a failed hold; says whether it put the key down.
    pub(super) fn failed(&mut self, rule: Rule, now: i64) -> bool {
        self.last_failure = Some(now);
        self.rule = rule;
        if self.down_since.is_some() {
            return false;
        }
        self.in_a_row += 1;
        if self.in_a_row < rule.failures {
            return false;
        }
        self.down_since = Some(now);
        self.tripped_at = self.in_a_row;
        self.pause(now);
        true
    }

    /// A probe failed, or ran past its time: the key stays down another
    /// pause, from now.
    pub(super) fn probe_failed(&mut self, now: i64) {
        self.last_failure = Some(now);
        self.pause(now);
    }

    /// The probe's holder went without saying: the next ask goes through.
    pub(super) fn probe_abandoned(&mut self) {
        self.probing = false;
    }

    fn pause(&mut self, now: i64) {
        self.resumes_at = now.saturating_add(millis(self.rule.pause));
        self.probing = false;
    }

    /// Whether the key is down: in its pause, or waiting for a probe to say.
    pub(super) fn down(&self) -> bool {
        self.down_since.is_some()
    }

    pub(super) fn down_since(&self) -> Option<i64> {
        self.down_since
    }

    pub(super) fn last_failure(&self) -> Option<i64> {
        self.last_failure
    }

    /// What every ask of a down key is answered.
    pub(super) fn refusal(&self, since: i64, now: i64) -> Error {
        let failures = self.tripped_at;
        let state = if now < self.resumes_at {
            let left = Duration::from_millis(u64::try_from(self.resumes_at - now).unwrap_or(0));
            format!("its pause ends in {left:?}")
        } else {
            "one hold tries it now".to_owned()
        };
        let what = format!("down since {}, after {failures} failed holds in a row: {state}", utc(since));
        Error::new(ErrorKind::Unavailable, what)
    }
}

/// How a semaphore's key stands now: whether what it guards is up, and how
/// many wait for it and hold it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct KeyState {
    /// False from the failure that paused the key until a hold after the
    /// pause succeeds; always true without a breaker.
    pub up: bool,
    pub down_since: Option<std::time::SystemTime>,
    /// The last failed hold's end, kept until a success after it.
    pub last_failure: Option<std::time::SystemTime>,
    pub waiting: u32,
    pub holding: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    const RULE: Rule = Rule { failures: 2, pause: Duration::from_secs(30) };

    #[test]
    fn two_failures_in_a_row_pause_the_key_and_one_ask_probes_once_the_pause_is_over() {
        let mut breaker = Breaker::new(RULE);
        assert!(!breaker.failed(RULE, 1_000));
        assert!(matches!(breaker.admit(1_500), Admission::Open), "one failure of two");
        assert!(breaker.failed(RULE, 2_000), "the second puts it down");
        let Admission::Refused(error) = breaker.admit(31_999) else { panic!("refused in its pause") };
        assert_eq!(error.kind(), ErrorKind::Unavailable);
        assert!(error.to_string().contains("after 2 failed holds in a row: its pause ends in 1ms"), "{error}");
        assert!(matches!(breaker.admit(32_000), Admission::Probe), "the pause is over");
        let Admission::Refused(error) = breaker.admit(32_001) else { panic!("one probe at a time") };
        assert!(error.to_string().contains("one hold tries it now"), "{error}");
        breaker.probe_failed(33_000);
        assert!(matches!(breaker.admit(62_999), Admission::Refused(_)), "paused again from the probe's failure");
        assert!(matches!(breaker.admit(63_000), Admission::Probe));
        breaker.probe_abandoned();
        assert!(matches!(breaker.admit(63_001), Admission::Probe), "its holder went: the next ask probes");
        assert_eq!(breaker.down_since(), Some(2_000));
        assert_eq!(breaker.last_failure(), Some(33_000));
    }
}
