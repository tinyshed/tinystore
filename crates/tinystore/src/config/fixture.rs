//! What the config's tests share: the environment they give a config and the
//! files they write.

use std::path::{Path, PathBuf};

use super::Env;

/// These variables with the prefix `APP`, the only environment a config reads.
pub(super) fn env(variables: &[(&str, &str)]) -> Env {
    Env::prefix("APP").from(variables.iter().map(|&(name, value)| (name, value)))
}

/// A file of `text` named `name` in `dir`.
pub(super) fn written(dir: &Path, name: &str, text: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, text).unwrap();
    path
}
