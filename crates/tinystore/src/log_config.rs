//! A store's logger reads its options from the config `log`, so that a
//! change of its level reaches the logger of every process of the store at
//! once. The code's options are the declaration's defaults, and the
//! variables the logger read, `LOG_LEVEL`, `LOG_FORMAT` and `LOG_TIME`, go
//! over them; only the level is live. The level is the least kept, and the
//! least printed unless the code's `level` has a `console` part, which is one
//! process's and no setting here. Records and kv's config meet here, so that
//! neither imports the other.
//!
//! ```text
//! level   live, one of trace, debug, info, warn, error   the code's, or trace: every level
//! format  one of pretty, json, off                       the code's, or what its terminal chose
//! time    one of clock, full, off                        the code's, or clock
//! ```

use std::any::Any;
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value, json};

use crate::config::Sent;
use crate::console::{Console, FORMATS, LEVELS, TIMES};
use crate::{Result, Store};

/// The config every store's logger reads.
const NAME: &str = "log";

/// Opens the config `log` as `console`'s logger declares it and follows it,
/// each value's level the console's from then on, until what it returns is
/// dropped.
pub(crate) fn follow(store: &Store, console: &Arc<Console>) -> Result<Arc<dyn Any + Send + Sync>> {
    let declared = console.declared();
    let declaration = json!({
        "level": { "kind": "text", "oneOf": one_of(&LEVELS, declared.level), "default": declared.level, "live": true },
        "format": { "kind": "text", "oneOf": one_of(&FORMATS, declared.format), "default": declared.format },
        "time": { "kind": "text", "oneOf": one_of(&TIMES, declared.time), "default": declared.time },
    });
    let layers = match &declared.env {
        None => json!([]),
        Some((prefix, read)) => {
            let variables: Map<String, Value> =
                read.iter().map(|(name, text)| (name.clone(), Value::String(text.clone()))).collect();
            json!([{ "env": { "prefix": prefix, "variables": variables } }])
        }
    };
    let sent = Sent { name: NAME, declaration: &declaration, layers: &layers, strict: false, remember: true };
    let config = store.config_sent(&sent)?;
    let followed = Arc::clone(console);
    let watching = config.watch(move |value, _| {
        if let Some(level) = value.get("level").and_then(Value::as_str) {
            followed.set_level(level);
        }
    });
    // The mutex makes `Watching` Sync, which the store's `dyn Any + Send + Sync` asks.
    Ok(Arc::new((config, Mutex::new(watching))))
}

/// The names a setting allows, its default first, as every SDK's `oneOf` lists them.
fn one_of<T>(table: &[(&'static str, T)], default: &'static str) -> Vec<&'static str> {
    let others = table.iter().map(|(name, _)| *name).filter(|name| *name != default);
    std::iter::once(default).chain(others).collect()
}

#[cfg(test)]
#[path = "log_config_tests.rs"]
mod tests;
