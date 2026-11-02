//! The tool `search_list`: the store's search indexes and what each holds.

use std::sync::Arc;

use serde_json::{Value, json};

use super::store::{Opener, opened, schema};
use super::tool::Tool;

pub(super) fn tools(open: &Opener) -> Vec<Tool> {
    let open = Arc::clone(open);
    let description = "The store's search indexes, with their documents.";
    vec![Tool::reads("search_list", description, schema(json!({}), &[]), move |_| {
        let listed = crate::search::list(&opened(&open)?)?;
        let shown: Vec<Value> = listed
            .iter()
            .map(|index| json!({ "name": index.name, "documents": index.documents, "vectors": index.vectors }))
            .collect();
        Ok(Value::Array(shown))
    })]
}
