//! The times a text spells, kept as how far each lies behind its event's
//! time, with the layout that spells it again; the text keeps the rest:
//!
//! ```text
//! event     2026-09-23 00:47:32.101187376
//! text      I20260923 00:47:32.100929   141 raft_server.h:60] Peer refresh succeeded!
//! stamp     1 byte in, "YMD h:m:s.6", 258 µs behind
//! rest      I   141 raft_server.h:60] Peer refresh succeeded!
//! ```
//!
//! The digits of a time are what zstd cannot squeeze, and they repeat what
//! the event's own time says already. A stamp is read as if it were UTC: a
//! program that writes its local time is behind by its zone, the same for
//! every line.
//!
//! A layout is the fields a stamp spells, in order, and the bytes between
//! them as they are: a field is a byte below 0x20, which a spelled byte never
//! is. A stamp is cut only where its layout spells the same bytes again.

use std::collections::HashMap;

use super::bytes::{Reader, put_count};
use super::head::Codec;
use super::ints::{put_ints, read_ints};
use super::text::{Decoder, Keep, put_texts, read_texts, wants};
use crate::Result;

const YEAR: u8 = 1; // four digits
const MONTH: u8 = 2; // two digits
const MONTH_NAME: u8 = 3; // Jan to Dec
const DAY: u8 = 4; // two digits
const DAY_SPACED: u8 = 5; // " 5" or "25", as syslog spells it
const HOUR: u8 = 6;
const MINUTE: u8 = 7;
const SECOND: u8 = 8;
const FRACTION: u8 = 9; // then how many digits it has, one to nine

/// The stamps one value keeps, and the bytes a stamp may start after the
/// value's start or the stamp before it.
const MOST_STAMPS: usize = 4;
const REACH: usize = 64;
/// The layouts one column holds, and the bytes of one.
const MOST_LAYOUTS: usize = 64;
const MOST_LAYOUT: usize = 64;
/// A fraction's digits at most: nanoseconds.
pub(super) const MOST_DIGITS: u8 = 9;
const SECONDS_IN_DAY: i64 = 86_400;
const NANOS_IN_DAY: i64 = SECONDS_IN_DAY * 1_000_000_000;
/// The fewest bytes a pattern spells: "0923 00:47:32".
const SHORTEST: usize = 13;

/// The layouts a stamp is looked for in, each perhaps followed by a fraction.
const PATTERNS: [&[u8]; 8] = [
    &[YEAR, b'-', MONTH, b'-', DAY, b' ', HOUR, b':', MINUTE, b':', SECOND], // Python, log4j, Postgres
    &[YEAR, b'-', MONTH, b'-', DAY, b'T', HOUR, b':', MINUTE, b':', SECOND], // RFC 3339
    &[YEAR, b'/', MONTH, b'/', DAY, b' ', HOUR, b':', MINUTE, b':', SECOND], // Go's log
    &[YEAR, MONTH, DAY, b' ', HOUR, b':', MINUTE, b':', SECOND],             // glog with its year
    &[MONTH, DAY, b' ', HOUR, b':', MINUTE, b':', SECOND],                   // glog
    &[DAY, b' ', MONTH_NAME, b' ', YEAR, b' ', HOUR, b':', MINUTE, b':', SECOND], // Redis
    &[MONTH_NAME, b' ', DAY_SPACED, b' ', HOUR, b':', MINUTE, b':', SECOND], // syslog
    &[DAY, b'/', MONTH_NAME, b'/', YEAR, b':', HOUR, b':', MINUTE, b':', SECOND], // access logs
];

const MONTHS: [&[u8]; 12] =
    [b"Jan", b"Feb", b"Mar", b"Apr", b"May", b"Jun", b"Jul", b"Aug", b"Sep", b"Oct", b"Nov", b"Dec"];
/// The letters a month's name begins with.
const MONTH_INITIALS: &[u8] = b"ADFJMNOS";

/// One time a text spells: where, in which pattern and fraction, and the
/// time as if it were UTC, counted in the unit its fraction counts.
#[derive(Clone, Copy)]
struct Stamp {
    start: usize,
    end: usize,
    pattern: usize,
    /// Before the fraction; zero without one.
    separator: u8,
    digits: u8,
    wall: i64,
}

impl Stamp {
    fn layout(&self) -> Vec<u8> {
        let mut layout = PATTERNS[self.pattern].to_vec();
        if self.separator != 0 {
            layout.extend_from_slice(&[self.separator, FRACTION, self.digits]);
        }
        layout
    }
}

/// A date and a time of day as a stamp spells them.
#[derive(Default)]
struct Civil {
    year: i64,
    month: i64,
    day: i64,
    hour: i64,
    minute: i64,
    second: i64,
    fraction: i64,
}

impl Civil {
    /// Takes the fields of a layout at `text[from..]`, and says where they
    /// end.
    fn read(&mut self, text: &[u8], from: usize, layout: &[u8]) -> Option<usize> {
        let mut at = from;
        for field in layout {
            let (value, next) = read_field(text, at, *field)?;
            match *field {
                YEAR => self.year = value,
                MONTH | MONTH_NAME => self.month = value,
                DAY | DAY_SPACED => self.day = value,
                HOUR => self.hour = value,
                MINUTE => self.minute = value,
                SECOND => self.second = value,
                _ => {}
            }
            at = next;
        }
        Some(at)
    }

    /// A day the calendar has, at a time of day a clock shows; a leap second
    /// is not one, and stays text.
    fn valid(&self) -> bool {
        let clock = self.hour <= 23 && self.minute <= 59 && self.second <= 59;
        let day = (1..=12).contains(&self.month) && self.day >= 1;
        clock
            && day
            && civil_from_days(days_from_civil(self.year, self.month, self.day)) == (self.year, self.month, self.day)
    }

    /// The time as if it were UTC, in units of a fraction of `digits` digits;
    /// none past what sixty-four bits of those units hold.
    fn wall(&self, digits: u8) -> Option<i64> {
        let of_day = self.hour * 3600 + self.minute * 60 + self.second;
        let seconds =
            days_from_civil(self.year, self.month, self.day).checked_mul(SECONDS_IN_DAY)?.checked_add(of_day)?;
        seconds.checked_mul(pow10(digits))?.checked_add(self.fraction)
    }
}

fn read_field(text: &[u8], at: usize, field: u8) -> Option<(i64, usize)> {
    match field {
        YEAR => read_digits(text, at, 4),
        MONTH_NAME => {
            let month = MONTHS.iter().position(|name| text[at..].starts_with(name))?;
            Some((month as i64 + 1, at + 3))
        }
        DAY_SPACED => match (text.get(at), text.get(at + 1)) {
            (Some(b' '), Some(digit @ b'1'..=b'9')) => Some((i64::from(digit - b'0'), at + 2)),
            (Some(b'0'), _) => None,
            _ => read_digits(text, at, 2),
        },
        MONTH | DAY | HOUR | MINUTE | SECOND => read_digits(text, at, 2),
        // a byte between fields, spelled as it is
        _ => (text.get(at) == Some(&field)).then_some((0, at + 1)),
    }
}

fn read_digits(text: &[u8], at: usize, width: usize) -> Option<(i64, usize)> {
    let digits = text.get(at..at + width)?;
    let value =
        digits.iter().try_fold(0, |value, digit| digit.is_ascii_digit().then(|| value * 10 + i64::from(digit - b'0')));
    Some((value?, at + width))
}

/// The proleptic Gregorian calendar as days since 1970-01-01, and back:
///
/// ```text
/// 1970-01-01 → 0      2000-03-01 → 11017      1969-12-31 → -1
/// ```
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted + 2) / 5 + 1;
    let month = if shifted + 3 > 12 { shifted - 9 } else { shifted + 3 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

fn pow10(digits: u8) -> i64 {
    10_i64.pow(u32::from(digits))
}

/// How far a time counted in a fraction of `digits` digits lies behind an
/// event's time, and back, the time that lies so far behind it:
///
/// ```text
/// at 1727300000123456789 ns, a stamp of 1727300000121 in milliseconds → 2 behind
/// ```
pub(super) fn behind(at: i64, wall: i64, digits: u8) -> Option<i64> {
    at.div_euclid(pow10(MOST_DIGITS - digits)).checked_sub(wall)
}

/// The stamps of `text`, each starting within reach of the end of the one
/// before; `at` is its event's time, whose year a stamp without one has.
fn find_stamps(found: &mut Vec<Stamp>, text: &[u8], at: i64) {
    let mut from = 0;
    for _ in 0..MOST_STAMPS {
        let Some(next) = next_stamp(text, from, at) else { break };
        from = next.end;
        found.push(next);
    }
}

/// Tries the patterns at each place a stamp could start: those that begin
/// with a digit at a digit, the one that begins with a month's name at its
/// initial.
fn next_stamp(text: &[u8], from: usize, at: i64) -> Option<Stamp> {
    let last = (text.len() + 1).saturating_sub(SHORTEST).min(from + REACH);
    for start in from..last {
        let by_name = MONTH_INITIALS.contains(&text[start]);
        if !by_name && !text[start].is_ascii_digit() {
            continue;
        }
        let tried = PATTERNS.iter().enumerate().filter(|(_, layout)| (layout[0] == MONTH_NAME) == by_name);
        if let Some(found) = tried.filter_map(|(pattern, layout)| read_stamp(text, start, pattern, layout, at)).next() {
            return Some(found);
        }
    }
    None
}

/// Reads a stamp in one pattern at `text[start..]`, and the fraction after
/// it, and keeps it only when it is a time a calendar and a clock can show.
fn read_stamp(text: &[u8], start: usize, pattern: usize, layout: &[u8], at: i64) -> Option<Stamp> {
    let mut spelled = Civil::default();
    let mut found = Stamp { start, end: spelled.read(text, start, layout)?, pattern, separator: 0, digits: 0, wall: 0 };
    let fraction =
        matches!(text.get(found.end), Some(b'.' | b',')) && text.get(found.end + 1).is_some_and(u8::is_ascii_digit);
    if fraction {
        found.separator = text[found.end];
        // up to nine digits; any more stay in the text
        for digit in
            text[found.end + 1..].iter().take(usize::from(MOST_DIGITS)).take_while(|digit| digit.is_ascii_digit())
        {
            spelled.fraction = spelled.fraction * 10 + i64::from(digit - b'0');
            found.digits += 1;
        }
        found.end += 1 + usize::from(found.digits);
    }
    if !layout.contains(&YEAR) {
        spelled.year = civil_from_days(at.div_euclid(NANOS_IN_DAY)).0;
    }
    found.wall = spelled.valid().then(|| spelled.wall(found.digits))??;
    Some(found)
}

/// A column's values cut apart: the rest of each, and its stamps.
#[derive(Default)]
struct Cut {
    layouts: Vec<Vec<u8>>,
    /// How many stamps each value holds.
    counts: Vec<i64>,
    /// Each stamp's layout, by its place among the column's.
    ids: Vec<i64>,
    /// The bytes of the rest between a stamp and the one before it in its
    /// value.
    gaps: Vec<i64>,
    /// How far a stamp lies behind its event's time, in its unit.
    behind: Vec<i64>,
    rest: Vec<String>,
}

/// Appends `values` with the times they spell cut out of them, when those
/// pay for the columns they take: eight at least, one a value in eight.
/// `times` are their events'. It says whether it wrote.
pub(super) fn put_stamped(out: &mut Vec<u8>, values: &[&str], times: &[i64], codec: &mut Codec) -> Result<bool> {
    let (mut stamps, mut ends) = (Vec::new(), Vec::new());
    for (value, at) in values.iter().zip(times) {
        find_stamps(&mut stamps, value.as_bytes(), *at);
        ends.push(stamps.len());
    }
    if stamps.len() < 8 || stamps.len() * 8 < values.len() {
        return Ok(false);
    }
    let cut = cut(values, times, &stamps, &ends);
    put_count(out, cut.layouts.len() as u64);
    for layout in &cut.layouts {
        put_count(out, layout.len() as u64);
        out.extend_from_slice(layout);
    }
    for ints in [&cut.counts, &cut.ids, &cut.gaps, &cut.behind] {
        put_ints(out, ints);
    }
    put_texts(out, &cut.rest.iter().map(String::as_str).collect::<Vec<_>>(), codec)?;
    Ok(true)
}

/// Takes each value's stamps out of it. A stamp whose layout the column has
/// no room for, or that lies further behind than sixty-four bits hold, stays
/// text, and so do the stamps after it in its value.
fn cut(values: &[&str], times: &[i64], stamps: &[Stamp], ends: &[usize]) -> Cut {
    let mut cut = Cut::default();
    let mut known: HashMap<(usize, u8, u8), usize> = HashMap::new();
    let mut first = 0;
    for ((value, at), end) in values.iter().zip(times).zip(ends) {
        let (mut rest, mut from, mut kept) = (String::with_capacity(value.len()), 0, 0);
        for stamp in &stamps[first..*end] {
            let key = (stamp.pattern, stamp.separator, stamp.digits);
            let layout = match known.get(&key) {
                Some(layout) => Some(*layout),
                None if cut.layouts.len() < MOST_LAYOUTS => {
                    cut.layouts.push(stamp.layout());
                    Some(*known.entry(key).or_insert(cut.layouts.len() - 1))
                }
                None => None,
            };
            let (Some(layout), Some(distance)) = (layout, behind(*at, stamp.wall, stamp.digits)) else { break };
            rest.push_str(&value[from..stamp.start]);
            cut.ids.push(layout as i64);
            cut.gaps.push((stamp.start - from) as i64);
            cut.behind.push(distance);
            from = stamp.end;
            kept += 1;
        }
        rest.push_str(&value[from..]);
        cut.rest.push(rest);
        cut.counts.push(kept);
        first = *end;
    }
    cut
}

/// The values `put_stamped` wrote, one a time of `times`, their events',
/// those `keep` drops left empty.
pub(super) fn read_stamped(
    reader: &mut Reader<'_>,
    times: &[i64],
    decoder: &mut Decoder<'_>,
    keep: Keep<'_>,
) -> Vec<String> {
    let layouts: Vec<&[u8]> = (0..reader.count(MOST_LAYOUTS)).map(|_| reader.counted(MOST_LAYOUT)).collect();
    if layouts.iter().any(|layout| layout_digits(layout).is_none()) {
        reader.fail("a stamp's layout");
    }
    let counts = read_ints(reader, times.len());
    if counts.iter().any(|count| !(0..=MOST_STAMPS as i64).contains(count)) {
        reader.fail("the stamps of one value");
        return Vec::new();
    }
    let total = counts.iter().sum::<i64>() as usize;
    let (ids, gaps, distances) = (read_ints(reader, total), read_ints(reader, total), read_ints(reader, total));
    let rest = read_texts(reader, times.len(), decoder, keep);
    let of = |id: &i64| usize::try_from(*id).ok().and_then(|id| layouts.get(id)).copied();
    let Some(spelling) = ids.iter().map(of).collect::<Option<Vec<&[u8]>>>() else {
        reader.fail("a stamp's layout past its column's");
        return Vec::new();
    };
    // what the stamps add to the text is counted before it is built
    let added: usize = spelling.iter().map(|layout| layout_width(layout)).sum();
    if added > decoder.left || rest.len() != times.len() || gaps.len() != total || distances.len() != total {
        reader.fail("stamps past what their block may expand to");
        return Vec::new();
    }
    decoder.left -= added;
    let mut stamps = spelling.iter().zip(gaps.iter().zip(&distances));
    let rebuilt = rest.iter().zip(times).zip(&counts).enumerate().map(|(place, ((rest, at), count))| {
        if !wants(keep, place) {
            (0..*count).try_for_each(|_| stamps.next().map(drop))?;
            return Some(String::new());
        }
        let (mut value, mut from) = (String::with_capacity(rest.len() + 32), 0);
        for _ in 0..*count {
            let (layout, (gap, distance)) = stamps.next()?;
            let to = from + usize::try_from(*gap).ok()?;
            value.push_str(rest.get(from..to)?);
            spell(&mut value, layout, *at, *distance)?;
            from = to;
        }
        value.push_str(rest.get(from..)?);
        Some(value)
    });
    rebuilt.collect::<Option<Vec<String>>>().unwrap_or_else(|| {
        reader.fail("a stamp's place or time");
        Vec::new()
    })
}

/// A layout's fraction digits, when the layout is one: fields below 0x20,
/// the fraction once at most, with its count, and nothing else below 0x20.
fn layout_digits(layout: &[u8]) -> Option<u8> {
    let (mut digits, mut fractions, mut at) = (0, 0, 0);
    while at < layout.len() {
        match layout[at] {
            FRACTION => {
                digits = *layout.get(at + 1).filter(|digits| (1..=MOST_DIGITS).contains(digits))?;
                (fractions, at) = (fractions + 1, at + 1);
            }
            field if field < YEAR || (field > SECOND && field < 0x20) || field >= 0x80 => return None,
            _ => {}
        }
        at += 1;
    }
    (!layout.is_empty() && fractions <= 1).then_some(digits)
}

/// The bytes a layout spells.
fn layout_width(layout: &[u8]) -> usize {
    let (mut width, mut at) = (0, 0);
    while at < layout.len() {
        width += match layout[at] {
            YEAR => 4,
            MONTH_NAME => 3,
            MONTH | DAY | DAY_SPACED | HOUR | MINUTE | SECOND => 2,
            FRACTION => {
                at += 1;
                usize::from(layout.get(at).copied().unwrap_or(0))
            }
            _ => 1,
        };
        at += 1;
    }
    width
}

/// Appends the time that lies `distance` behind `at`, as `layout` spells it;
/// none for a year four digits cannot spell.
fn spell(out: &mut String, layout: &[u8], at: i64, distance: i64) -> Option<()> {
    let digits = layout_digits(layout)?;
    let wall = at.div_euclid(pow10(MOST_DIGITS - digits)).checked_sub(distance)?;
    let seconds = wall.div_euclid(pow10(digits));
    let fraction = wall - seconds * pow10(digits);
    let (days, of_day) = (seconds.div_euclid(SECONDS_IN_DAY), seconds.rem_euclid(SECONDS_IN_DAY));
    let (year, month, day) = civil_from_days(days);
    let mut place = 0;
    while place < layout.len() {
        match layout[place] {
            YEAR if !(0..=9999).contains(&year) => return None,
            YEAR => padded(out, year, 4),
            MONTH => padded(out, month, 2),
            MONTH_NAME => MONTHS[(month - 1) as usize].iter().for_each(|letter| out.push(char::from(*letter))),
            DAY => padded(out, day, 2),
            DAY_SPACED if day < 10 => {
                out.push(' ');
                padded(out, day, 1);
            }
            DAY_SPACED => padded(out, day, 2),
            HOUR => padded(out, of_day / 3600, 2),
            MINUTE => padded(out, of_day / 60 % 60, 2),
            SECOND => padded(out, of_day % 60, 2),
            FRACTION => {
                place += 1;
                padded(out, fraction, u32::from(digits));
            }
            literal => out.push(char::from(literal)),
        }
        place += 1;
    }
    Some(())
}

/// Appends a value of at most `width` digits, zeros first.
fn padded(out: &mut String, value: i64, width: u32) {
    (0..width).rev().for_each(|place| out.push(char::from(b'0' + (value / 10_i64.pow(place) % 10) as u8)));
}

#[cfg(test)]
#[path = "stamp_tests.rs"]
mod tests;
