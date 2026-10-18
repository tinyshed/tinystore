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

/// Rust's strict and reserved keywords that a raw identifier may name.
const KEYWORDS: [&str; 46] = [
    "abstract", "as", "async", "await", "become", "box", "break", "const", "continue", "do", "dyn", "else", "enum",
    "extern", "false", "final", "fn", "for", "gen", "if", "impl", "in", "let", "loop", "macro", "match", "mod", "move",
    "mut", "override", "priv", "pub", "ref", "return", "static", "struct", "trait", "true", "try", "type", "typeof",
    "unsafe", "unsized", "use", "virtual", "where",
];

/// A field as Rust names it: a keyword escaped, and one no raw identifier
/// may name, `self` and its kin, given a trailing `_`.
pub(crate) fn rust_field(name: &str) -> String {
    let snake = snake(name);
    match snake.as_str() {
        "self" | "super" | "crate" => format!("{snake}_"),
        "while" | "yield" => format!("r#{snake}"),
        keyword if KEYWORDS.contains(&keyword) => format!("r#{snake}"),
        _ => snake,
    }
}

/// A field, or a function of a message's, as Python names it: in snake case.
/// A keyword stays as it is, since a field is a key of a dict.
pub(crate) fn python_field(name: &str) -> String {
    snake(name)
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
