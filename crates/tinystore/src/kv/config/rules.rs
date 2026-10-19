//! A value checked against its setting, whichever layer gave it: its kind,
//! then the rules of one value. A variable's text is read as its setting's
//! kind first.

use serde_json::{Map, Number, Value};

use super::declaration::{Group, Item, Kind, Node, Rules, Setting, did_you_mean, joined, meant, shown};

/// The units of a span, longest first, as the SDKs spell one: `1h30m`, `250ms`.
const UNITS: [(&str, u64); 6] =
    [("w", 604_800_000), ("d", 86_400_000), ("h", 3_600_000), ("m", 60_000), ("s", 1_000), ("ms", 1)];

/// A value a layer gave `setting`, as the setting keeps it, or why it is
/// refused: `70000 is above 65535`.
pub(crate) fn check(setting: &Setting, value: &Value) -> Result<Value, String> {
    let value = check_kind(&setting.kind, value)?;
    check_rules(&setting.kind, &setting.rules, &value)?;
    Ok(value)
}

fn check_kind(kind: &Kind, value: &Value) -> Result<Value, String> {
    match (kind, value) {
        (Kind::Text, Value::String(_)) | (Kind::Boolean, Value::Bool(_)) | (Kind::Object, Value::Object(_)) => {
            Ok(value.clone())
        }
        (Kind::Any, _) => Ok(value.clone()),
        (Kind::Number, Value::Number(_)) => Ok(value.clone()),
        (Kind::Integer, Value::Number(number)) => {
            whole(number).ok_or_else(|| format!("{} is not a whole number", shown(value)))
        }
        (Kind::Duration, Value::String(text)) => span(text).map(Value::from),
        (Kind::Duration, Value::Number(number)) => {
            number.as_u64().map(Value::from).ok_or_else(|| format!("{} is not a span", shown(value)))
        }
        (Kind::List(item), Value::Array(items)) => items
            .iter()
            .enumerate()
            .map(|(at, value)| check_item(item, value).map_err(|why| format!("[{at}]{why}")))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        _ => Err(format!("{} is not {}", shown(value), described(kind))),
    }
}

/// A number that is whole and fits 64 bits, as a whole number.
fn whole(number: &Number) -> Option<Value> {
    if number.is_i64() || number.is_u64() {
        return Some(Value::Number(number.clone()));
    }
    let float = number.as_f64()?;
    // 2^63 reads back as itself from the saturated i64::MAX, so the range is
    // checked before the cast rather than by reading the cast back.
    const BOUND: f64 = 9_223_372_036_854_775_808.0;
    if float.fract() != 0.0 || !(-BOUND..BOUND).contains(&float) {
        return None;
    }
    #[expect(clippy::cast_possible_truncation, reason = "a whole float within i64's range casts exactly")]
    let int = float as i64;
    Some(Value::from(int))
}

/// What an item of a list was given, with the place of a field inside it:
/// `.host is required`, which the list's index goes before.
fn check_item(item: &Item, value: &Value) -> Result<Value, String> {
    match item {
        Item::Value { kind, rules } => {
            let value = check_kind(kind, value).map_err(|why| format!(": {why}"))?;
            check_rules(kind, rules, &value).map_err(|why| format!(": {why}"))?;
            Ok(value)
        }
        Item::Group(group) => match value {
            Value::Object(map) => check_group(group, map, ""),
            other => Err(format!(": {} is not a group of settings", shown(other))),
        },
    }
}

/// An item's group: every key one of its settings, every required one there,
/// the others' defaults filled in where the item leaves them out.
fn check_group(group: &Group, map: &Map<String, Value>, path: &str) -> Result<Value, String> {
    if let Some(key) = map.keys().find(|key| !group.contains_key(*key)) {
        let near = meant(key, group.keys()).map(|near| joined(path, &near));
        return Err(format!(".{}: no such setting{}", joined(path, key), did_you_mean(near)));
    }
    let mut checked = Map::new();
    for (key, node) in group {
        let at = joined(path, key);
        let given = map.get(key).filter(|value| !value.is_null());
        let value = match (node, given) {
            (Node::Group(inner), Some(Value::Object(fields))) => check_group(inner, fields, &at)?,
            (Node::Group(_), Some(other)) => return Err(format!(".{at}: {} is not a group of settings", shown(other))),
            (Node::Group(inner), None) => check_group(inner, &Map::new(), &at)?,
            (Node::Setting(setting), Some(value)) => check(setting, value).map_err(|why| format!(".{at}: {why}"))?,
            (Node::Setting(setting), None) if setting.required => return Err(format!(".{at} is required")),
            (Node::Setting(setting), None) => match &setting.default {
                Some(default) => default.clone(),
                None => continue,
            },
        };
        checked.insert(key.clone(), value);
    }
    Ok(Value::Object(checked))
}

fn check_rules(kind: &Kind, rules: &Rules, value: &Value) -> Result<(), String> {
    if let (Some(allowed), Value::String(text)) = (&rules.one_of, value)
        && !allowed.contains(text)
    {
        return Err(format!("{} is not one of {}", shown(value), allowed.join(", ")));
    }
    let measured = match (kind, value) {
        (Kind::List(_), Value::Array(items)) => Some(items.len() as f64),
        (_, Value::Number(number)) => number.as_f64(),
        _ => None,
    };
    let Some(measured) = measured else { return Ok(()) };
    let spelled = |bound: f64| spelled_bound(kind, bound);
    if let Some(min) = rules.min.filter(|&min| measured < min) {
        return Err(format!("{} is below {}", spelled(measured), spelled(min)));
    }
    if let Some(max) = rules.max.filter(|&max| measured > max) {
        return Err(format!("{} is above {}", spelled(measured), spelled(max)));
    }
    Ok(())
}

/// A bound as an error says it: a span spelled, a list's length in items.
fn spelled_bound(kind: &Kind, bound: f64) -> String {
    match kind {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a span's bound is whole milliseconds"
        )]
        Kind::Duration => spelled(bound as u64),
        Kind::List(_) => format!("{bound} items"),
        _ => bound.to_string(),
    }
}

pub(super) fn described(kind: &Kind) -> &'static str {
    match kind {
        Kind::Text => "text",
        Kind::Integer => "a whole number",
        Kind::Number => "a number",
        Kind::Boolean => "true or false",
        Kind::Duration => "a span, as 30s or 1h30m",
        Kind::List(_) => "a list",
        Kind::Object => "a map",
        Kind::Any => "a value",
    }
}

/// A variable's text as `setting` keeps it: `"3000"` a number, `"a.com,b.com"`
/// a list, `"1h30m"` a span.
pub(crate) fn parse(setting: &Setting, text: &str) -> Result<Value, String> {
    let value = parse_kind(&setting.kind, text)?;
    check(setting, &value)
}

fn parse_kind(kind: &Kind, text: &str) -> Result<Value, String> {
    let trimmed = text.trim();
    let refused = || format!("{text:?} is not {}", described(kind));
    match kind {
        Kind::Text => Ok(Value::String(text.to_owned())),
        Kind::Integer => match trimmed.parse::<i64>() {
            Ok(int) => Ok(Value::from(int)),
            Err(_) if trimmed.parse::<f64>().is_ok() => Err(refused()),
            Err(_) => Err(format!("{text:?} is not a number")),
        },
        // `PORT=9000` for a TypeScript program's `port: 8080` stays 9000 and is
        // never shown as 9000.0
        Kind::Number => match trimmed.parse::<i64>() {
            Ok(whole) => Ok(Value::from(whole)),
            Err(_) => trimmed.parse::<f64>().ok().and_then(Number::from_f64).map(Value::Number).ok_or_else(refused),
        },
        Kind::Boolean => boolean(trimmed).map(Value::Bool).ok_or_else(refused),
        Kind::Duration => span(trimmed).map(Value::from),
        Kind::List(item) => list(item, trimmed),
        Kind::Object => serde_json::from_str::<Map<String, Value>>(trimmed).map(Value::Object).map_err(|_| refused()),
        Kind::Any => Ok(serde_json::from_str(trimmed).unwrap_or_else(|_| Value::String(text.to_owned()))),
    }
}

/// `true` or `false` as Go's `strconv.ParseBool` reads them, as variables of
/// the Go releases were read.
fn boolean(text: &str) -> Option<bool> {
    match text {
        "1" | "t" | "T" | "true" | "TRUE" | "True" => Some(true),
        "0" | "f" | "F" | "false" | "FALSE" | "False" => Some(false),
        _ => None,
    }
}

/// A list given as JSON, or as items between commas: `a.com, b.com`.
fn list(item: &Item, text: &str) -> Result<Value, String> {
    if text.starts_with('[') {
        return serde_json::from_str(text).map_err(|error| format!("{text:?} is not a list: {error}"));
    }
    let Item::Value { kind, .. } = item else {
        return Err(format!("{text:?} is not a list: a list of groups is given as JSON"));
    };
    if text.is_empty() {
        return Ok(Value::Array(Vec::new()));
    }
    text.split(',').map(|part| parse_kind(kind, part.trim())).collect::<Result<Vec<_>, _>>().map(Value::Array)
}

/// A span's milliseconds: `1h30m` is 5,400,000 and `250ms` 250, its units each
/// once and longest first.
pub(crate) fn span(text: &str) -> Result<u64, String> {
    let refused = || format!("{text:?} is not a span, as 30s or 1h30m");
    let mut rest = text;
    let (mut total, mut next_unit) = (0_u64, 0);
    while !rest.is_empty() {
        let digits = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
        let count: u64 = rest[..digits].parse().map_err(|_| refused())?;
        rest = &rest[digits..];
        let (at, &(unit, millis)) = UNITS
            .iter()
            .enumerate()
            .skip(next_unit)
            .find(|(_, (unit, _))| starts_unit(rest, unit))
            .ok_or_else(refused)?;
        rest = &rest[unit.len()..];
        next_unit = at + 1;
        let past = || format!("{text:?} is past the milliseconds 64 bits hold");
        total = count.checked_mul(millis).and_then(|part| total.checked_add(part)).ok_or_else(past)?;
    }
    if text.is_empty() { Err(refused()) } else { Ok(total) }
}

/// Whether `rest` begins with `unit` and not a longer one: `m` is not the
/// start of `ms`.
fn starts_unit(rest: &str, unit: &str) -> bool {
    rest.starts_with(unit) && !(unit == "m" && rest.starts_with("ms"))
}

/// Milliseconds as a span is written: 5,400,000 is `1h30m`, 0 is `0s`.
pub(crate) fn spelled(millis: u64) -> String {
    if millis == 0 {
        return "0s".to_owned();
    }
    let mut left = millis;
    let mut text = String::new();
    for (unit, size) in UNITS {
        if left >= size {
            text.push_str(&format!("{}{unit}", left / size));
            left %= size;
        }
    }
    text
}

#[cfg(test)]
#[path = "rules_tests.rs"]
mod tests;
