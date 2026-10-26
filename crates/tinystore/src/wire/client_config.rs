//! A shell's change of a config's live settings, `config update` and `config
//! reset`, on the config as the store keeps it: the last declaration a
//! program opened it with, and no layer of the program's own.

use std::io::{Read, Write};

use super::Client;
use crate::Result;
use crate::wire::protocol::{self as wire, method};

impl<S: Read + Write> Client<S> {
    /// Opens the config `name` as the store keeps it: its handle and the
    /// version of its changes.
    pub fn config(&mut self, name: &str) -> Result<(u64, i64)> {
        let open = wire::ConfigOpen { name: name.to_owned(), kept: true, ..wire::ConfigOpen::default() };
        let value = self.answer::<wire::ConfigValue>(method::CONFIG_OPEN, &open)?;
        Ok((value.handle, value.version))
    }

    /// Changes the live settings `changes` names, JSON in the declaration's
    /// shape, while the config is still at `version`: the version it made.
    pub fn update_config(&mut self, config: u64, version: i64, changes: &str) -> Result<i64> {
        let update = wire::ConfigUpdate { handle: config, version, changes: changes.to_owned() };
        Ok(self.answer::<wire::ConfigValue>(method::CONFIG_UPDATE, &update)?.version)
    }

    /// Drops the changes kept of the setting or group at `path`, or of every
    /// setting when it is empty: the version it made.
    pub fn reset_config(&mut self, config: u64, path: &str) -> Result<i64> {
        let reset = wire::ConfigReset { handle: config, path: path.to_owned() };
        Ok(self.answer::<wire::ConfigValue>(method::CONFIG_RESET, &reset)?.version)
    }
}
