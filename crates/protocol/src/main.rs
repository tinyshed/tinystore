//! Writes the wire protocol's codecs and vectors from protocol/*.wire, the one
//! place a message is declared. `protocol --check` writes nothing and fails
//! when a file is not what the schema writes, as CI runs it.
//!
//! | Module                          | What it does with a `.wire` file                     |
//! |---------------------------------|------------------------------------------------------|
//! | `syntax`                        | reads its text into lines: the language's one parser |
//! | `format`                        | prints the lines back in the language's one layout   |
//! | `schema`                        | joins every file's lines and checks them together    |
//! | `rust`, `typescript`, `vectors` | write a codec, or the vectors, from the schema       |

#[cfg(test)]
mod agreement_tests;
mod format;
mod names;
mod rust;
mod schema;
mod syntax;
mod typescript;
mod vectors;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use schema::Schema;

fn main() -> ExitCode {
    let check = std::env::args().any(|arg| arg == "--check");
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    match run(&root, check) {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            eprintln!("protocol: {failure}");
            ExitCode::FAILURE
        }
    }
}

fn run(root: &Path, check: bool) -> Result<(), String> {
    let (schema, mut stale) = read(&root.join("protocol"), check)?;
    let outputs = [
        ("crates/tinystore/src/wire/protocol.rs", rust::write(&schema)),
        ("sdk/js/src/wire/protocol.ts", typescript::write(&schema)),
        ("testdata/wire/protocol.json", vectors::write(&schema)),
    ];
    for (name, text) in outputs {
        let path = root.join(name);
        let current = fs::read_to_string(&path).unwrap_or_default();
        if current == text {
            continue;
        }
        if check {
            stale.push(name.to_owned());
        } else {
            fs::write(&path, text).map_err(|error| format!("{name}: {error}"))?;
            println!("wrote {name}");
        }
    }
    match stale.is_empty() {
        true => Ok(()),
        false => Err(format!("not what protocol/*.wire writes; run `just protocol`:\n  {}", stale.join("\n  "))),
    }
}

/// Every schema file, in the order of their names, read once: its lines make
/// the schema, and are laid out as `format` prints them, the file written
/// again or, when checking, named among those that are not laid out.
fn read(dir: &Path, check: bool) -> Result<(Schema, Vec<String>), String> {
    let mut schema = Schema::default();
    let mut stale = Vec::new();
    for file in wire_files(dir)? {
        let name = format!("protocol/{}", file.file_name().unwrap_or_default().to_string_lossy());
        let text = fs::read_to_string(&file).map_err(|error| format!("{name}: {error}"))?;
        let lines = syntax::parse(&text).map_err(|(line, why)| format!("{name}:{line}: {why}"))?;
        let laid = format::print(&lines);
        if laid != text && check {
            stale.push(name);
        } else if laid != text {
            fs::write(&file, laid).map_err(|error| format!("{name}: {error}"))?;
            println!("laid out {name}");
        }
        schema.add(&lines);
    }
    schema.check()?;
    Ok((schema, stale))
}

/// The schema files, in the order of their names.
fn wire_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(|error| format!("{}: {error}", dir.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|extension| extension == "wire"))
        .collect();
    files.sort();
    Ok(files)
}
