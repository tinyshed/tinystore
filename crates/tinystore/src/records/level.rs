//! The level of another program's line, found where the programs that write
//! lines put it, as slog numbers it:
//!
//! ```text
//! {"level":50,…}                                   pino and bunyan: 50 → error
//! {"level":"warn",…}   level=warn msg=…            JSON and logfmt, by name
//! E0926 12:00:01.000000  1 main.go:12] …           glog's first letter
//! 1:M 26 Sep 2026 12:00:01.500 # WARNING …         Redis's mark after its time
//! 2026-09-26 12:00:01,500 ERROR [main] …           log4j, Python, Postgres: the first level word
//! 2026/09/26 06:12:02 [Warning] core: started      a level in brackets or in colour, in any case
//! ```

use super::logfmt::unquote;
use super::stamp::redis_stamp_end;

const TRACE: i32 = -8;
const DEBUG: i32 = -4;
const INFO: i32 = 0;
const WARN: i32 = 4;
const ERROR: i32 = 8;
const FATAL: i32 = 12;
/// How far into a line a level word is looked for.
const REACH: usize = 96;
/// The keys a JSON or logfmt line names its level with.
const KEYS: [&str; 5] = ["level", "lvl", "severity", "levelname", "log.level"];

/// A line's level, from its fields when it has them and from its text
/// otherwise.
pub(super) fn level_of(text: &str, fields: Option<&[(String, String)]>) -> Option<i32> {
    if let Some(fields) = fields {
        let (_, value) = fields.iter().find(|(key, _)| KEYS.contains(&key.as_str()))?;
        return value_level(value);
    }
    let text = text.as_bytes();
    glog_level(text).or_else(|| redis_level(text)).or_else(|| {
        let first = text.split(|byte| *byte == b'\n').next().unwrap_or_default();
        logfmt_level(first).or_else(|| word_level(&first[..first.len().min(REACH)]))
    })
}

/// The level of a name programs give one, in any case; zerolog's console
/// writes three letters.
fn named(word: &[u8]) -> Option<i32> {
    match word.to_ascii_lowercase().as_slice() {
        b"trace" | b"trc" => Some(TRACE),
        b"debug" | b"dbg" => Some(DEBUG),
        b"info" | b"inf" | b"notice" | b"log" => Some(INFO),
        b"warn" | b"wrn" | b"warning" => Some(WARN),
        b"error" | b"err" | b"severe" => Some(ERROR),
        b"fatal" | b"ftl" | b"critical" | b"crit" | b"panic" | b"alert" | b"emerg" => Some(FATAL),
        _ => None,
    }
}

/// A level written as a name, or as pino's number:
///
/// ```text
/// 10 trace   20 debug   30 info   40 warn   50 error   60 fatal
/// ```
fn value_level(value: &str) -> Option<i32> {
    if let Some(name) = unquote(value) {
        return named(name.as_bytes());
    }
    let number: i64 = value.parse().ok()?;
    Some(match number {
        ..20 => TRACE,
        20..30 => DEBUG,
        30..40 => INFO,
        40..50 => WARN,
        50..60 => ERROR,
        _ => FATAL,
    })
}

/// The letter glog begins a line with, before the digits of its date.
fn glog_level(text: &[u8]) -> Option<i32> {
    if text.len() < 2 || !text[1].is_ascii_digit() {
        return None;
    }
    match text[0] {
        b'I' => Some(INFO),
        b'W' => Some(WARN),
        b'E' => Some(ERROR),
        b'F' => Some(FATAL),
        _ => None,
    }
}

/// The mark Redis writes after its time: `.` and `-` for debug, `*` for
/// notice, `#` for warning.
///
/// ```text
/// 1:M 26 Sep 2026 12:00:01.500 * Ready to accept connections   → info
/// ```
fn redis_level(text: &[u8]) -> Option<i32> {
    let role = text.iter().position(|byte| *byte == b':')?;
    let process = role >= 1 && text[..role].iter().all(u8::is_ascii_digit);
    if !process || role + 3 > text.len() || !b"MCSX".contains(&text[role + 1]) || text[role + 2] != b' ' {
        return None;
    }
    let end = redis_stamp_end(text, role + 3)?;
    let mark = end + 1;
    if mark + 1 >= text.len() || text[end] != b' ' || text[mark + 1] != b' ' {
        return None;
    }
    match text[mark] {
        b'.' | b'-' => Some(DEBUG),
        b'*' => Some(INFO),
        b'#' => Some(WARN),
        _ => None,
    }
}

/// The value of the first `level=` or `lvl=` pair a line begins or has after a
/// space.
fn logfmt_level(line: &[u8]) -> Option<i32> {
    for key in [&b"level="[..], b"lvl="] {
        // `log_level=debug level=warn`: the first `level=` ends a word, the second begins one.
        let begins_a_word = |at: &usize| *at == 0 || line[*at - 1] == b' ';
        let mut found = line.windows(key.len()).enumerate().filter(|(_, window)| *window == key).map(|(at, _)| at);
        let Some(at) = found.find(begins_a_word) else { continue };
        let value = &line[at + key.len()..];
        let value = &value[..value.iter().position(|byte| matches!(byte, b' ' | b'\t')).unwrap_or(value.len())];
        let start = value.iter().position(|byte| *byte != b'"').unwrap_or(value.len());
        let end = value.iter().rposition(|byte| *byte != b'"').map_or(start, |last| last + 1);
        return named(&value[start..end.max(start)]);
    }
    None
}

/// The first level a line names: a word in capitals, or a name in brackets or
/// in colour in any case. Postgres's LOG counts only with its colon, unless it
/// is in colour.
fn word_level(head: &[u8]) -> Option<i32> {
    let mut start = 0;
    while start < head.len() {
        let end = start + head[start..].iter().take_while(|byte| byte.is_ascii_alphabetic()).count();
        if end == start {
            start += escape_length(&head[start..]).max(1);
            continue;
        }
        let word = &head[start..end];
        if let Some(level) = named(word)
            && (marked(head, start, end) || is_level_word(word, &head[end..]))
        {
            return Some(level);
        }
        start = end;
    }
    None
}

fn is_level_word(word: &[u8], after: &[u8]) -> bool {
    word.len() >= 3 && word.iter().all(u8::is_ascii_uppercase) && (word != b"LOG" || after.starts_with(b":"))
}

/// A word in brackets, or between colour codes, spaces aside:
///
/// ```text
/// [warn]      \x1b[33mwarn\x1b[0m      \x1b[36m    LOG\x1b[0m
/// ```
fn marked(head: &[u8], start: usize, end: usize) -> bool {
    if start > 0 && head[start - 1] == b'[' && head.get(end) == Some(&b']') {
        return true;
    }
    let before = &head[..head[..start].iter().rposition(|byte| *byte != b' ').map_or(0, |last| last + 1)];
    let Some(escape) = before.iter().rposition(|byte| *byte == 0x1b) else { return false };
    escape_length(&before[escape..]) == before.len() - escape && escape_length(&head[end..]) > 0
}

/// How long the terminal control sequence `text` begins with is, a colour's
/// included; zero when it begins with none.
///
/// ```text
/// \x1b[1;32m → 7
/// ```
fn escape_length(text: &[u8]) -> usize {
    if text.len() < 3 || text[0] != 0x1b || text[1] != b'[' {
        return 0;
    }
    for (at, byte) in text.iter().enumerate().skip(2) {
        match byte {
            0x40..=0x7e => return at + 1,
            0x20..=0x3f => {}
            _ => return 0,
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::console::object_pairs;

    #[test]
    fn a_lines_level_is_found_where_its_program_writes_it() {
        let lines = [
            (r#"{"level":50,"time":1727300000121,"msg":"failed"}"#, Some(ERROR)),
            (r#"{"level":"warn","msg":"slow"}"#, Some(WARN)),
            (r#"level=warn msg="slow request" ms=1200"#, Some(WARN)),
            ("ts=2026-09-26T12:00:01Z lvl=error msg=boom", Some(ERROR)),
            ("log_level=debug level=warn msg=slow", Some(WARN)),
            ("E0926 12:00:01.000000  1 main.go:12] boom", Some(ERROR)),
            ("I20260923 00:47:32.100929   141 raft_server.h:60] ok", Some(INFO)),
            ("1:M 26 Sep 2026 12:00:01.500 # WARNING Memory overcommit", Some(WARN)),
            ("1:C 26 Sep 2026 12:00:01.500 * DB saved on disk", Some(INFO)),
            ("2026-09-26 12:00:01,500 ERROR [main] boom", Some(ERROR)),
            ("2026-09-21 21:09:00,391 - aiohttp.access - INFO - GET /health", Some(INFO)),
            ("2026-09-11 13:14:49.437 UTC [48] LOG:  listening", Some(INFO)),
            ("2026-09-11 13:14:49.437 UTC [48] FATAL:  terminating", Some(FATAL)),
            ("2026/09/24 06:12:02.759278 [Warning] core: Xray started", Some(WARN)),
            ("2026/09/26 12:00:01 [error] 7#7: *1 open() failed", Some(ERROR)),
            ("+0300 2026-09-26 12:00:01 \x1b[32mINFO\x1b[0m [main] started", Some(INFO)),
            ("[2026-09-26 12:00:03] \x1b[32minfo\x1b[39m (app): ready", Some(INFO)),
            ("2026-09-26 12:00:01.500 \x1b[36m    LOG\x1b[0m checkpoint", Some(INFO)),
            ("2026-09-26 12:00:01 INF Established secure connection", Some(INFO)),
            ("2026-09-26 12:00:01 WRN Failed TLS handshake", Some(WARN)),
            (
                "logger=search namespace=default group=dashboards resource=folders t=2026-09-26T12:00:01Z level=info",
                Some(INFO),
            ),
            ("connection reset by peer", None),
            ("no error occurred", None),
            ("LOG rotated", None),
            ("a \x1b[1mbold\x1b[0m word, then error", None),
            ("1:M 26 Sep 2026 12:00:01.500 ? unknown", None),
        ];
        for (line, expected) in lines {
            let fields = line.starts_with('{').then(|| object_pairs(line).unwrap());
            assert_eq!(level_of(line, fields.as_deref()), expected, "{line:?}");
        }
    }

    #[test]
    fn a_colour_marks_a_level_as_brackets_do() {
        for (head, start, end) in
            [("[warn] x", 1, 5), ("\x1b[33mwarn\x1b[0m x", 5, 9), ("\x1b[36m    LOG\x1b[0m x", 9, 12)]
        {
            assert!(marked(head.as_bytes(), start, end), "{head:?}");
        }
        assert!(!marked(b"a \x1b[1mbold word", 7, 11), "a word after a colour and before none");
        assert_eq!(escape_length(b"\x1b[1;32mINFO"), 7);
        assert_eq!(escape_length(b"\x1b[1;32"), 0, "an unfinished colour");
    }
}
