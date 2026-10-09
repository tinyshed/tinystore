//! How a schema's names are spelled in each language.
//!
//! ```text
//! kv.Call          → KvCall           a type
//! kv.rateLimit.open → KV_RATE_LIMIT_OPEN a Rust constant
//! expiresAt        → expires_at       a Rust field
//! ```

/// A message's type: each dotted part capitalized and joined.
pub(crate) fn type_name(name: &str) -> String {
    name.split('.').map(capitalized).collect()
}

/// A method's Rust constant: each part's words in capitals, joined by `_`.
pub(crate) fn const_name(name: &str) -> String {
    name.split('.').map(|part| snake(part).to_uppercase()).collect::<Vec<_>>().join("_")
}

/// A field as Rust names it, a keyword escaped.
pub(crate) fn rust_field(name: &str) -> String {
    let snake = snake(name);
    match snake.as_str() {
        "type" | "match" | "move" | "ref" | "in" | "loop" | "use" | "where" | "self" => format!("r#{snake}"),
        _ => snake,
    }
}

fn capitalized(part: &str) -> String {
    let mut chars = part.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn snake(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for (index, char) in name.chars().enumerate() {
        if char.is_uppercase() {
            if index > 0 {
                out.push('_');
            }
            out.extend(char.to_lowercase());
        } else {
            out.push(char);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_is_spelled_as_each_language_spells_it() {
        assert_eq!(type_name("kv.Call"), "KvCall");
        assert_eq!(type_name("Hello"), "Hello");
        assert_eq!(const_name("kv.rateLimit.open"), "KV_RATE_LIMIT_OPEN");
        assert_eq!(rust_field("expiresAt"), "expires_at");
        assert_eq!(rust_field("type"), "r#type");
    }
}
