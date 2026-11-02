//! What stands for a store and its kv.db with `config-load` alone, which
//! builds neither: types with no value, so that every config is loaded
//! without a store and none of these calls is ever made. With `config`,
//! `store.rs` takes their place.

use std::sync::Arc;

use serde_json::Value;

use super::hub::{Hub, Kept, Written};
use crate::Result;

pub(super) enum Store {}

pub(super) enum Keeper {}

impl Hub {
    pub(super) fn of(store: &Store, _name: &str) -> Result<Arc<Hub>> {
        match *store {}
    }
}

impl Keeper {
    pub(super) fn change(&self, _expected: i64, _leaves: Vec<(String, Value)>) -> Result<Written> {
        match *self {}
    }

    pub(super) fn reset(&self, _path: &str) -> Result<(Kept, bool)> {
        match *self {}
    }

    pub(super) fn remember(&self, _text: String) -> Result<()> {
        match *self {}
    }
}
