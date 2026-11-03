//! A program's settings as one typed value, declared in layers: the
//! declaration in code, then files and the environment, each declaring
//! again what the ones before it gave; and, for the settings marked `Live`,
//! changes made while the program runs, kept in kv.db, over every layer
//! until they are reset, and seen by every handle at once. The book is
//! `docs/guides/config/README.md`.
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
//!
//! A program that keeps no live setting reads its settings with
//! [`load`], with no store and nothing written to disk. The `config-load`
//! feature builds `load` alone, with no store and no SQLite; `config` adds
//! `store.config` and the live settings.
//!
//! A program that learns the shape of some settings only as it runs, such
//! as a plugin's, declares them as data, a [`Declaration`], and reads them
//! as a `serde_json::Value`.

mod data;
mod declaration;
mod env;
mod explain;
mod files;
mod handle;
mod hub;
mod layers;
mod marks;
#[cfg(not(feature = "config"))]
mod no_store;
mod resolve;
mod rules;
#[cfg(feature = "config")]
mod store;

use std::fmt;

use serde::Serialize;
use serde::de::DeserializeOwned;

pub use declaration::Declaration;
pub use env::Env;
pub use explain::{Explained, Given, Line, Origin};
pub use files::File;
pub use handle::{Config, IfVersion, Snapshot, Watching};
use layers::Layer;
pub use marks::{Entries, Live, Required, Secret};
use resolve::Validate;
// The store a config opens in, and kv.db, which keeps its changes there:
// `config-load` alone builds neither, and loads every config without them.
#[cfg(not(feature = "config"))]
use no_store::{Keeper, Store};
#[cfg(feature = "config")]
use {crate::Store, store::Keeper};
// What kv keeps of the configs open, and what the wire, the CLI and a store's logger open.
#[cfg(feature = "config")]
pub(crate) use hub::Hub;
#[cfg(all(feature = "config", any(test, feature = "wire", feature = "records")))]
pub(crate) use store::Sent;
#[cfg(all(feature = "config", feature = "client"))]
pub(crate) use store::names;

use crate::Result;

/// A config being opened: its name, its declaration, and its layers in the
/// order the program writes them, each declaring again what came before.
#[must_use = "a config opens with open()"]
pub struct ConfigBuilder<T> {
    /// The store whose kv.db keeps the live settings' changes; none for a
    /// config loaded without one.
    store: Option<Store>,
    name: String,
    declaration: Declared<T>,
    layers: Vec<Layer>,
    validate: Option<Validate<T>>,
    strict: bool,
}

/// What a config is declared with: a value of the program's type, whose
/// settings it is read as, or a [`Declaration`] as data, whose settings are
/// read as a `serde_json::Value`.
#[diagnostic::on_unimplemented(
    message = "a config is declared by a serde value that is Clone, Send and Sync, or by a config::Declaration",
    label = "neither a Serialize + DeserializeOwned + Clone + Send + Sync value nor a Declaration"
)]
pub trait IntoDeclaration {
    /// What the settings are read as.
    type Value: Serialize + DeserializeOwned + Clone + Send + Sync + 'static;

    #[doc(hidden)]
    fn into_declared(self) -> Declared<Self::Value>;
}

impl<T> IntoDeclaration for T
where
    T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    type Value = T;

    fn into_declared(self) -> Declared<T> {
        Declared(Source::Typed(self))
    }
}

impl IntoDeclaration for Declaration {
    type Value = serde_json::Value;

    fn into_declared(self) -> Declared<serde_json::Value> {
        Declared(Source::Data(self))
    }
}

/// A declaration as a config's builder keeps it until it opens.
#[doc(hidden)]
pub struct Declared<T>(Source<T>);

enum Source<T> {
    /// A value of the program's type, read with its marks when the config opens.
    Typed(T),
    Data(Declaration),
}

impl<T> fmt::Debug for Declared<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Source::Typed(_) => f.write_str("Declared(a typed value)"),
            Source::Data(declaration) => f.debug_tuple("Declared").field(declaration).finish(),
        }
    }
}

/// A program's settings read once from its declaration, its files and the
/// environment, with no store: the same layers, rules, secrets and errors as
/// `store.config`, and no directory, no file and no `kv.db`. A `Live` field
/// is refused when it opens, naming it, since only a store keeps a change.
///
/// ```no_run
/// use serde::{Deserialize, Serialize};
/// use tinystore::config::{Env, File, Secret};
///
/// #[derive(Clone, Default, Serialize, Deserialize)]
/// #[serde(default)]
/// struct Agent {
///     hub: String,
///     database: Secret<String>,
/// }
///
/// # fn main() -> tinystore::Result<()> {
/// let agent = tinystore::config::load("agent", Agent::default())
///     .file(File::optional("agent.toml"))
///     .env(Env::prefix("AGENT"))
///     .open()?;
/// let hub = agent.get().hub.clone();
/// # Ok(())
/// # }
/// ```
pub fn load<D: IntoDeclaration>(name: &str, declaration: D) -> ConfigBuilder<D::Value> {
    let declaration = declaration.into_declared();
    ConfigBuilder { store: None, name: name.to_owned(), declaration, layers: Vec::new(), validate: None, strict: false }
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

impl<T> fmt::Debug for ConfigBuilder<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConfigBuilder")
            .field("name", &self.name)
            .field("layers", &self.layers)
            .field("strict", &self.strict)
            .finish_non_exhaustive()
    }
}

#[cfg(all(test, feature = "config"))]
#[path = "config_tests.rs"]
mod tests;
