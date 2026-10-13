//! A program's settings as one typed value, declared in layers: the
//! declaration in code, then files and the environment, each declaring
//! again what the ones before it gave; and, for the settings marked `Live`,
//! changes made while the program runs, kept in kv.db, over every layer
//! until they are reset, and seen by every handle at once. The book is
//! `plan/api/config.md`.
//!
//! ```no_run
//! use serde::{Deserialize, Serialize};
//! use tinystore::config::{Env, File, Live, Secret};
//!
//! #[derive(Clone, Serialize, Deserialize)]
//! #[serde(default)]
//! struct Settings {
//!     port: u16,
//!     database: Secret<String>,
//!     rps: Live<u32>,
//! }
//!
//! impl Default for Settings {
//!     fn default() -> Self {
//!         Settings { port: 8080, database: Secret::default(), rps: Live::new(100) }
//!     }
//! }
//!
//! # fn main() -> tinystore::Result<()> {
//! let store = tinystore::Store::open("data", Default::default())?;
//! let config = store
//!     .config("app", Settings::default())
//!     .file(File::optional("app.toml"))
//!     .env(Env::prefix("APP").var("port", "PORT"))
//!     .open()?;
//! let port = config.get().port; // 8080, app.toml's, or PORT's
//! config.update(|settings| *settings.rps = 200)?; // kept, and every handle sees it
//! # Ok(())
//! # }
//! ```

#[cfg(any(test, feature = "wire"))]
mod data;
mod declaration;
mod env;
mod explain;
mod files;
mod handle;
mod hub;
mod layers;
mod marks;
mod resolve;
mod rules;

use std::fmt;

use serde::Serialize;
use serde::de::DeserializeOwned;

pub use env::Env;
pub use explain::{Explained, Line};
pub use files::File;
pub use handle::{Config, IfVersion, Snapshot, Watching};
pub(crate) use hub::Hub;
use layers::Layer;
pub use marks::{Live, Required, Secret};
use resolve::Validate;

use crate::{Result, Store};

/// A config being opened: its name, its declaration, and its layers in the
/// order the program writes them, each declaring again what came before.
#[must_use = "a config opens with open()"]
pub struct ConfigBuilder<T> {
    store: Store,
    name: String,
    declaration: T,
    layers: Vec<Layer>,
    validate: Option<Validate<T>>,
    strict: bool,
}

impl Store {
    /// The program's settings as one typed value. `declaration` is every
    /// setting with its default, a `serde` struct whose fields may be marked
    /// `Secret`, `Required` or `Live`; the layers the builder adds go over it
    /// in the order written.
    pub fn config<T>(&self, name: &str, declaration: T) -> ConfigBuilder<T>
    where
        T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
    {
        ConfigBuilder {
            store: self.clone(),
            name: name.to_owned(),
            declaration,
            layers: Vec::new(),
            validate: None,
            strict: false,
        }
    }
}

impl<T> ConfigBuilder<T>
where
    T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    /// A file's settings over what came before: TOML, YAML or JSON by its
    /// extension, refused when it does not exist unless `File::optional`.
    pub fn file(mut self, file: impl Into<File>) -> Self {
        self.layers.push(Layer::File(file.into()));
        self
    }

    /// The environment's variables over what came before: `Env::prefix("APP")`,
    /// or `"APP"`.
    pub fn env(mut self, env: impl Into<Env>) -> Self {
        self.layers.push(Layer::Env(env.into()));
        self
    }

    /// The program's own check of the whole value, at open and before a
    /// change is kept, for what relates two settings; it says why it refuses.
    pub fn validate(
        mut self,
        validate: impl Fn(&T) -> std::result::Result<(), String> + Send + Sync + 'static,
    ) -> Self {
        self.validate = Some(Box::new(validate));
        self
    }

    /// Refuses to open while a kept change no longer fits, rather than
    /// ignoring it and saying so in the log.
    pub fn strict(mut self) -> Self {
        self.strict = true;
        self
    }

    pub fn open(self) -> Result<Config<T>> {
        handle::open(self)
    }
}

/// A config as a program's SDK opens it over the wire: its declaration and
/// its layers as data, as `data` reads them.
#[cfg(any(test, feature = "wire"))]
pub(crate) struct Sent<'a> {
    pub(crate) name: &'a str,
    pub(crate) declaration: &'a serde_json::Value,
    pub(crate) layers: &'a serde_json::Value,
    pub(crate) strict: bool,
    /// Whether kv.db keeps the declaration: not for a connection that may
    /// only read.
    pub(crate) remember: bool,
}

#[cfg(any(test, feature = "wire"))]
impl Store {
    /// Opens the config an SDK sent, its value given back as data.
    pub(crate) fn config_sent(&self, sent: &Sent<'_>) -> Result<Config<serde_json::Value>> {
        let within = |why: String| crate::Error::invalid(why).within(format!("config {}", sent.name));
        let declaration = data::declaration(sent.declaration).map_err(within)?;
        let layers = data::layers(sent.layers).map_err(within)?;
        handle::open_with(handle::Opening {
            store: self.clone(),
            name: sent.name.to_owned(),
            declaration,
            layers,
            validate: None,
            strict: sent.strict,
            view: resolve::View::Data,
            remember: sent.remember,
        })
    }
}

impl<T> fmt::Debug for ConfigBuilder<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConfigBuilder")
            .field("name", &self.name)
            .field("layers", &self.layers)
            .field("strict", &self.strict)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
