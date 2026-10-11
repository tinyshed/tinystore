//! Where a database's migrations come from, and how their names number them.

use std::borrow::Cow;
use std::fs;
use std::path::{Path, PathBuf};

use crate::sqlite::Migration;
use crate::{Error, Result};

/// A database's migrations: a directory of `.sql` files named
/// `0001_notes.sql`, `0002_tags.sql`, applied in the order of their numbers,
/// or the files themselves by name, for a program that carries them in its
/// binary:
///
/// ```no_run
/// # fn main() -> tinystore::Result<()> {
/// # let store = tinystore::Store::open("data", Default::default())?;
/// let db = store.database("app").migrations("migrations").open()?;
/// let db = store
///     .database("app")
///     .migrations([("0001_notes.sql", "create table notes (id text primary key, title text not null) strict")])
///     .open()?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug)]
pub enum Migrations {
    Dir(PathBuf),
    Files(Vec<(String, String)>),
}

impl From<&str> for Migrations {
    fn from(dir: &str) -> Migrations {
        Migrations::Dir(PathBuf::from(dir))
    }
}

impl From<&Path> for Migrations {
    fn from(dir: &Path) -> Migrations {
        Migrations::Dir(dir.to_owned())
    }
}

impl From<PathBuf> for Migrations {
    fn from(dir: PathBuf) -> Migrations {
        Migrations::Dir(dir)
    }
}

impl<const N: usize> From<[(&str, &str); N]> for Migrations {
    fn from(files: [(&str, &str); N]) -> Migrations {
        Migrations::Files(files.iter().map(|(name, sql)| ((*name).to_owned(), (*sql).to_owned())).collect())
    }
}

impl From<Vec<(String, String)>> for Migrations {
    fn from(files: Vec<(String, String)>) -> Migrations {
        Migrations::Files(files)
    }
}

impl Migrations {
    /// The migrations in the order of their numbers, each numbered by its
    /// name. A history counts from 1 without a gap, which applying checks.
    pub(crate) fn load(self) -> Result<Vec<Migration>> {
        let files = match self {
            Migrations::Dir(dir) => read_dir(&dir)?,
            Migrations::Files(files) => files,
        };
        let numbered = |(name, sql): (String, String)| {
            Ok(Migration { version: number(&name)?, name: Cow::Owned(name), sql: Cow::Owned(sql) })
        };
        let mut migrations = files.into_iter().map(numbered).collect::<Result<Vec<Migration>>>()?;
        // by number and not by name, which puts 10_x.sql before 9_y.sql
        migrations.sort_by(|a, b| (a.version, &a.name).cmp(&(b.version, &b.name)));
        Ok(migrations)
    }
}

/// The `.sql` files at the root of `dir`, by name; anything else there, a
/// README, a directory, is not a migration.
fn read_dir(dir: &Path) -> Result<Vec<(String, String)>> {
    let what = || format!("migrations {}", dir.display());
    let entries = fs::read_dir(dir).map_err(|error| Error::io(what(), error))?;
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| Error::io(what(), error))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".sql") || !entry.path().is_file() {
            continue;
        }
        let sql = fs::read_to_string(entry.path()).map_err(|error| Error::io(format!("{}: {name}", what()), error))?;
        files.push((name, sql));
    }
    Ok(files)
}

/// The number a migration's name starts with: `0002_tags.sql` is 2.
fn number(name: &str) -> Result<u32> {
    let digits: &str = &name[..name.find(|c: char| !c.is_ascii_digit()).unwrap_or(name.len())];
    let numbered = !digits.is_empty() && name[digits.len()..].starts_with('_') && name.ends_with(".sql");
    match digits.parse() {
        Ok(number) if numbered => Ok(number),
        _ => Err(Error::invalid(format!(
            "migration {name}: a migration is named by its number and what it does, as 0001_notes.sql"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_migrations_name_numbers_it() {
        assert_eq!(number("0002_tags.sql").unwrap(), 2);
        assert_eq!(number("12_x.sql").unwrap(), 12);
        for name in ["tags.sql", "0002tags.sql", "_tags.sql", "0002_tags.txt"] {
            assert!(number(name).is_err(), "{name}");
        }
    }

    #[test]
    fn migrations_load_in_the_order_of_their_numbers_however_wide_each_is_written() {
        let files = ["10_tenth.sql", "9_ninth.sql", "0001_first.sql"].map(|name| (name.to_owned(), String::new()));
        let loaded = Migrations::Files(files.to_vec()).load().unwrap();
        let versions: Vec<u32> = loaded.iter().map(|migration| migration.version).collect();
        assert_eq!(versions, [1, 9, 10]);
    }
}
