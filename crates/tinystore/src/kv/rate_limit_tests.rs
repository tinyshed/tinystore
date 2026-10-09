use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;

use crate::ErrorKind;
use crate::kv::Allowance;
use crate::kv::{self, fixture::*};

fn allowed(left: u64) -> Allowance {
    Allowance { ok: true, left, retry_at: None, windows: Vec::new() }
}

#[test]
fn a_rate_limit_lets_a_burst_through_then_its_rate() {
    let f = fixture();
    let api = f.store.rate_limit("api").rate(3, 3 * SECOND).burst(3).open().unwrap().under("tenant-7");
    for left in [2, 1, 0] {
        assert_eq!(api.allow("user-1").unwrap(), allowed(left));
    }
    let refused = api.allow("user-1").unwrap();
    assert!(!refused.ok);
    assert_eq!(refused.retry_at, Some(f.store.now() + SECOND));
    assert!(api.allow("user-2").unwrap().ok, "another key has its own burst");

    f.clock.advance(SECOND);
    assert_eq!(api.allow("user-1").unwrap(), allowed(0), "a second later, one more");
    assert!(!api.allow("user-1").unwrap().ok, "but not two");
}

#[test]
fn requests_asked_together_pass_together_or_not_at_all() {
    let f = fixture();
    let uploads = f.store.rate_limit("uploads").rate(10, SECOND).open().unwrap();
    assert_eq!(uploads.allow_n("k", 7).unwrap(), allowed(3));
    let refused = uploads.allow_n("k", 4).unwrap();
    assert_eq!((refused.ok, refused.left), (false, 3));
    assert_eq!(refused.retry_at, Some(f.store.now() + std::time::Duration::from_millis(100)));
    assert_eq!(uploads.allow_n("k", 3).unwrap(), allowed(0));
    assert_eq!(uploads.allow_n("k", 11).unwrap_err().kind(), ErrorKind::Invalid, "past the burst never passes");
    assert_eq!(uploads.allow_n("k", 0).unwrap_err().kind(), ErrorKind::Invalid);
}

#[test]
fn a_rate_limits_times_outlive_a_restart() {
    let f = fixture();
    let login = f.store.rate_limit("login").rate(2, MINUTE).open().unwrap();
    assert!(login.allow("ip").unwrap().ok && login.allow("ip").unwrap().ok);
    let f = f.reopen();
    let login = f.store.rate_limit("login").rate(2, MINUTE).open().unwrap();
    let refused = login.allow("ip").unwrap();
    assert_eq!((refused.ok, refused.retry_at), (false, Some(f.store.now() + 30 * SECOND)));
}

#[test]
fn a_quiet_key_is_forgotten_once_its_time_has_come() {
    let f = fixture();
    let api = f.store.rate_limit("api").rate(1, SECOND).burst(5).open().unwrap();
    api.allow_n("k", 5).unwrap();
    kv::maintain(&f.store).unwrap();
    assert_eq!(f.rows("_tinystore_kv_cells"), 1);
    f.clock.advance(5 * SECOND);
    kv::maintain(&f.store).unwrap();
    assert_eq!(f.rows("_tinystore_kv_cells"), 0, "maintenance deleted the quiet key");
    assert_eq!(api.allow("k").unwrap(), allowed(4), "a whole burst again");
}

#[test]
fn requests_racing_for_a_key_pass_no_more_than_the_burst() {
    let f = fixture();
    let race = f.store.rate_limit("race").rate(50, HOUR).open().unwrap();
    let passed = AtomicU64::new(0);
    thread::scope(|scope| {
        for _ in 0..16 {
            scope.spawn(|| {
                for _ in 0..20 {
                    if race.allow("k").unwrap().ok {
                        passed.fetch_add(1, Ordering::SeqCst);
                    }
                }
            });
        }
    });
    assert_eq!(passed.load(Ordering::SeqCst), 50);
}

#[test]
fn a_rate_limit_without_a_rate_does_not_open() {
    let f = fixture();
    let cases = [
        f.store.rate_limit("bad"),
        f.store.rate_limit("bad").rate(0, SECOND),
        f.store.rate_limit("bad").rate(5, std::time::Duration::ZERO),
        f.store.rate_limit("bad").rate(2, std::time::Duration::from_nanos(1)),
        f.store.rate_limit("bad").rate(1, SECOND).burst(0),
        f.store.rate_limit("bad").rate(1, 100 * 365 * DAY),
    ];
    for builder in cases {
        let error = builder.open().unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Invalid, "{error}");
    }
    f.store.bucket::<i64>("taken").open().unwrap();
    assert!(f.store.rate_limit("taken").rate(1, SECOND).open().is_err(), "a bucket's name");
}

#[test]
fn peek_uses_nothing_and_reset_gives_a_key_its_whole_burst() {
    let f = fixture();
    let login = f.store.rate_limit("login").rate(3, MINUTE).open().unwrap();
    assert_eq!(login.peek("ip").unwrap(), allowed(2), "the answer one request would get");
    assert_eq!(login.peek("ip").unwrap(), allowed(2), "and nothing was used");
    login.allow_n("ip", 3).unwrap();
    assert!(!login.peek("ip").unwrap().ok);
    login.reset("ip").unwrap();
    assert_eq!(login.allow("ip").unwrap(), allowed(2));
}
