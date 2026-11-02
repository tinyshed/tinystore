//! What a person or an agent gives, read the same by tinystore's tools and by
//! the command line: a span of time, a name that may be misspelt, and a
//! database of the store's directory.

use std::path::Path;
use std::time::Duration;

/// A span as a person writes one: a whole number and its unit.
///
/// ```text
/// 30000ms → 30 s     30s → 30 s     5m → 300 s     2d → 172,800 s
/// ```
pub fn span(text: &str) -> Result<Duration, String> {
    let digits = text.find(|c: char| !c.is_ascii_digit()).unwrap_or(text.len());
    let (number, unit) = text.split_at(digits);
    let number: u64 = number.parse().map_err(|_| format!("the span {text}: a number and a unit, as 30s"))?;
    let millis = match unit {
        "ms" => 1,
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        "" if number == 0 => 0,
        _ => return Err(format!("the span {text}: a unit of ms, s, m, h or d")),
    };
    Ok(Duration::from_millis(number.saturating_mul(millis)))
}

/// What follows a refusal of a name that may be misspelt: the nearest of
/// `names`, one edit away for three to six letters and two for more, as the
/// command line reads a command. It is only said: no word is read as another.
pub(crate) fn meant(given: &str, names: &[&str]) -> String {
    let edits = match given.chars().count() {
        0..3 => 0,
        3..7 => 1,
        _ => 2,
    };
    crate::typos::Dictionary::new(names)
        .nearest(given, edits)
        .map_or_else(String::new, |near| format!("; did you mean {}?", near.word))
}

/// The application's databases by name, `sql/<name>.db`, in order.
pub fn databases(store: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(store.join("sql")) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().to_str()?.strip_suffix(".db").map(str::to_owned))
        .collect();
    names.sort();
    names
}

/// Refuses a database the store does not hold, naming the nearest it does:
/// opening one makes it, so a typo would otherwise make an empty one.
pub fn database_held(dir: &Path, database: &str) -> Result<(), String> {
    if dir.join("sql").join(format!("{database}.db")).exists() {
        return Ok(());
    }
    let known = databases(dir);
    let names: Vec<&str> = known.iter().map(String::as_str).collect();
    let held = if known.is_empty() { "none".to_owned() } else { known.join(", ") };
    Err(format!("{} holds no database {database}{}; it holds {held}", dir.display(), meant(database, &names)))
}

/// Why the store holds no `file` to read a bucket or a queue from: none was
/// kept yet, or one was opened from a database, whose file keeps it.
pub fn none_kept(store: &Path, file: &str, kept: &str) -> String {
    let databases = databases(store);
    if databases.is_empty() {
        return format!("{} holds no {file}: no program has kept a {kept} yet", store.display());
    }
    format!(
        "{} holds no {file}: a {kept} a program opened from a database is kept in it, read with its database, \
         one of {}",
        store.display(),
        databases.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_read_as_people_write_them() {
        assert_eq!(span("30000ms").unwrap(), Duration::from_secs(30));
        assert_eq!(span("2d").unwrap(), Duration::from_secs(172_800));
        assert!(span("30").is_err(), "a unit");
    }

    #[test]
    fn a_misspelt_name_is_told_its_nearest_and_a_short_one_none() {
        assert_eq!(meant("kv_sett", &["kv_set", "kv_delete"]), "; did you mean kv_set?");
        assert_eq!(meant("ap", &["app"]), "", "every short word is near another");
    }

    #[test]
    fn a_database_the_store_does_not_hold_is_named_with_those_it_does() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sql")).unwrap();
        std::fs::write(dir.path().join("sql/plants.db"), []).unwrap();
        assert_eq!(databases(dir.path()), ["plants"]);
        assert!(database_held(dir.path(), "plants").is_ok());
        let why = database_held(dir.path(), "plnts").unwrap_err();
        assert!(why.ends_with("holds no database plnts; did you mean plants?; it holds plants"), "{why}");
    }
}
