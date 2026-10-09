use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;

use crate::ErrorKind;
use crate::kv::Allowance;
use crate::kv::fixture::*;

/// Whether a use passed, how many more would, and each window's use, in the
/// order of the quota's windows.
fn expect(usage: &Allowance, ok: bool, left: u64, used: &[u64]) {
    assert_eq!((usage.ok, usage.left), (ok, left), "{usage:?}");
    let windows: Vec<u64> = usage.windows.iter().map(|window| window.used).collect();
    assert_eq!(windows, used, "{usage:?}");
    for window in &usage.windows {
        assert_eq!(window.left, window.limit - window.used, "{window:?}");
    }
}

#[test]
fn a_use_counts_in_every_window_or_in_none() {
    let f = fixture();
    let ai = f.store.quota("ai").window("session", 3, 5 * HOUR).window("weekly", 5, 7 * DAY).open().unwrap();
    for used in 1..=3 {
        expect(&ai.allow("user-1").unwrap(), true, 3 - used, &[used, used]);
    }
    let refused = ai.allow("user-1").unwrap();
    expect(&refused, false, 0, &[3, 3]);
    assert_eq!(refused.retry_at, Some(f.store.now() + 5 * HOUR), "until the session resets");

    f.clock.advance(5 * HOUR);
    for used in 1..=2 {
        expect(&ai.allow("user-1").unwrap(), true, 2 - used, &[used, used + 3]);
    }
    let refused = ai.allow("user-1").unwrap();
    expect(&refused, false, 0, &[2, 5]);
    assert_eq!(refused.retry_at, Some(f.store.now() + 7 * DAY - 5 * HOUR), "until the week resets");

    assert!(ai.allow("user-2").unwrap().ok, "another key");
    assert!(ai.under("tenant-7").allow("user-1").unwrap().ok, "the same key in another branch");
}

#[test]
fn a_window_starts_at_the_first_use_after_the_last_ended() {
    let f = fixture();
    let ai = f.store.quota("ai").window("session", 100, 5 * HOUR).open().unwrap();
    let start = f.store.now();
    let first = ai.allow("user-1").unwrap();
    assert_eq!(first.window("session").unwrap().resets_at, Some(start + 5 * HOUR));
    f.clock.advance(5 * HOUR + 25 * MINUTE);
    let next = ai.allow("user-1").unwrap();
    let session = next.window("session").unwrap();
    assert_eq!((session.used, session.resets_at), (1, Some(start + 10 * HOUR + 25 * MINUTE)));
}

#[test]
fn peek_counts_nothing_and_a_refund_never_goes_below_nothing() {
    let f = fixture();
    let tokens = f.store.quota("tokens").window("hour", 1000, HOUR).window("day", 5000, DAY).open().unwrap();
    let never_used = tokens.peek("user-1").unwrap();
    expect(&never_used, true, 1000, &[0, 0]);
    assert_eq!(never_used.window("hour").unwrap().resets_at, None, "a window not started");

    expect(&tokens.allow_n("user-1", 800).unwrap(), true, 200, &[800, 800]);
    expect(&tokens.allow_n("user-1", 300).unwrap(), false, 200, &[800, 800]);
    expect(&tokens.peek("user-1").unwrap(), true, 200, &[800, 800]);
    tokens.refund_n("user-1", 300).unwrap();
    tokens.refund_n("user-1", 600).unwrap();
    expect(&tokens.peek("user-1").unwrap(), true, 1000, &[0, 0]);
    assert_eq!(tokens.allow_n("user-1", 1001).unwrap_err().kind(), ErrorKind::Invalid);
    tokens.reset("user-1").unwrap();
    expect(&tokens.peek("user-1").unwrap(), true, 1000, &[0, 0]);
}

#[test]
fn a_quotas_windows_outlive_a_restart_and_follow_its_new_windows() {
    let f = fixture();
    let tokens = f.store.quota("tokens").window("hour", 1000, HOUR).window("day", 5000, DAY).open().unwrap();
    tokens.allow_n("user-1", 900).unwrap();
    let f = f.reopen();
    let tokens = f.store.quota("tokens").window("hour", 2000, HOUR).window("week", 9000, 7 * DAY).open().unwrap();
    expect(&tokens.peek("user-1").unwrap(), true, 1100, &[900, 0]);
    tokens.reset("user-1").unwrap();
    expect(&tokens.peek("user-1").unwrap(), true, 2000, &[0, 0]);
}

#[test]
fn uses_racing_for_a_key_pass_no_more_than_its_limit() {
    let f = fixture();
    let ai = f.store.quota("ai").window("hour", 10, HOUR).window("day", 25, DAY).open().unwrap();
    let passed = AtomicU64::new(0);
    thread::scope(|scope| {
        for _ in 0..40 {
            scope.spawn(|| {
                if ai.allow("user-1").unwrap().ok {
                    passed.fetch_add(1, Ordering::SeqCst);
                }
            });
        }
    });
    assert_eq!(passed.load(Ordering::SeqCst), 10);
}

#[test]
fn a_quota_that_cannot_count_does_not_open() {
    let f = fixture();
    let quota = || f.store.quota("bad");
    let hours = (b'a'..=b'i').fold(quota(), |quota, name| quota.window(&(name as char).to_string(), 1, HOUR));
    let cases = [
        quota(),
        quota().window("Session", 1, HOUR),
        quota().window("session", 0, HOUR),
        quota().window("session", 1, std::time::Duration::from_micros(1)),
        quota().window("session", 1, HOUR).window("session", 2, 2 * HOUR),
        hours,
    ];
    for builder in cases {
        let error = builder.open().unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Invalid, "{error}");
    }
    f.store.bucket::<String>("values").open().unwrap();
    assert!(f.store.quota("values").window("hour", 1, HOUR).open().is_err(), "a bucket's name");
}

#[test]
fn a_sign_in_admits_no_more_password_checks_than_its_quotas() {
    let f = fixture();
    let attempts =
        f.store.quota("signin-attempts").window("burst", 5, 15 * MINUTE).window("day", 50, DAY).open().unwrap();
    let (login, address) = (attempts.under("login"), attempts.under("address"));
    let checked = AtomicU64::new(0);
    thread::scope(|scope| {
        for _ in 0..20 {
            scope.spawn(|| {
                let admitted = login.allow("ann").unwrap().ok && address.allow("192.0.2.1").unwrap().ok;
                if admitted {
                    checked.fetch_add(1, Ordering::SeqCst);
                }
            });
        }
    });
    assert_eq!(checked.load(Ordering::SeqCst), 5, "twenty attempts ran five password checks");
}
