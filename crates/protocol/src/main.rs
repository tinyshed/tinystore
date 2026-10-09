//! Writes the wire protocol's codecs and vectors from protocol/*.wire, the one
//! place a message is declared. `protocol --check` writes nothing and fails
//! when a file is not what the schema writes, as CI runs it.

mod names;
mod rust;
mod schema;
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
    let schema = read(&root.join("protocol"))?;
    let outputs = [
        ("crates/tinystore/src/wire/protocol.rs", rust::write(&schema)),
        ("sdk/js/src/wire/protocol.ts", typescript::write(&schema)),
        ("testdata/wire/protocol.json", vectors::write(&schema)),
    ];
    let mut stale = Vec::new();
    for (name, text) in outputs {
        let path = root.join(name);
        let current = fs::read_to_string(&path).unwrap_or_default();
        if current == text {
            continue;
        }
        if check {
            stale.push(name);
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

/// Every schema file, in the order of their names.
fn read(dir: &Path) -> Result<Schema, String> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(|error| format!("{}: {error}", dir.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|extension| extension == "wire"))
        .collect();
    files.sort();
    let mut schema = Schema::default();
    for file in &files {
        let text = fs::read_to_string(file).map_err(|error| format!("{}: {error}", file.display()))?;
        let name = file.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        schema.parse(&name, &text).map_err(|refused| refused.to_string())?;
    }
    schema.check()?;
    Ok(schema)
}
