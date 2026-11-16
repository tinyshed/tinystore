//! The names a scan passes over: a file's or a folder's, `*` for any run of
//! characters within one name, every other character itself.
//!
//! ```text
//! "*.part" → "film.mkv.part", ".part"; not "film.part.mkv"
//! ".*"     → ".DS_Store", ".git"; not "a.b"
//! ```

use crate::{Error, Result};

/// Patterns a directory takes at most.
const MOST_PATTERNS: usize = 64;
/// A pattern's bytes at most.
const LONGEST_PATTERN: usize = 256;

#[derive(Debug, Default)]
pub(super) struct Ignore {
    patterns: Vec<String>,
}

impl Ignore {
    pub(super) fn new(patterns: Vec<String>) -> Result<Ignore> {
        if patterns.len() > MOST_PATTERNS {
            return Err(Error::invalid(format!("{} patterns to ignore, over {MOST_PATTERNS}", patterns.len())));
        }
        for pattern in &patterns {
            if pattern.is_empty() || pattern.len() > LONGEST_PATTERN {
                return Err(Error::invalid(format!("a pattern to ignore of {} bytes; 1 to 256", pattern.len())));
            }
            if pattern.contains(['/', '\\']) || pattern.chars().any(char::is_control) {
                return Err(Error::invalid(format!("a pattern to ignore is one name, and {pattern:?} is not")));
            }
        }
        Ok(Ignore { patterns })
    }

    pub(super) fn passes_over(&self, name: &str) -> bool {
        self.patterns.iter().any(|pattern| matches(pattern.as_bytes(), name.as_bytes()))
    }
}

/// Whether `name` matches `pattern`, whose `*` is any run of bytes: the
/// last `*` met is where a mismatch tries again, one byte further.
fn matches(pattern: &[u8], name: &[u8]) -> bool {
    let (mut p, mut n) = (0, 0);
    let mut retry: Option<(usize, usize)> = None;
    while n < name.len() {
        if p < pattern.len() && pattern[p] == b'*' {
            retry = Some((p, n));
            p += 1;
        } else if p < pattern.len() && pattern[p] == name[n] {
            p += 1;
            n += 1;
        } else if let Some((star, from)) = retry {
            p = star + 1;
            n = from + 1;
            retry = Some((star, from + 1));
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|&byte| byte == b'*')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_star_is_any_run_of_a_names_characters_and_the_rest_is_itself() {
        let cases = [
            ("*.part", "film.mkv.part", true),
            ("*.part", ".part", true),
            ("*.part", "film.part.mkv", false),
            (".*", ".DS_Store", true),
            (".*", "a.b", false),
            ("Thumbs.db", "Thumbs.db", true),
            ("Thumbs.db", "thumbs.db", false),
            ("a*b*c", "aXbYbZc", true),
            ("a*b*c", "aXbYbZ", false),
            ("*", "anything", true),
            ("**x", "x", true),
        ];
        for (pattern, name, matched) in cases {
            assert_eq!(matches(pattern.as_bytes(), name.as_bytes()), matched, "{pattern:?} against {name:?}");
        }
    }

    #[test]
    fn a_pattern_is_one_name_within_its_bounds() {
        for bad in ["", "a/b", "a\\b", "a\u{0}"] {
            assert!(Ignore::new(vec![bad.to_owned()]).is_err(), "{bad:?}");
        }
        assert!(Ignore::new(vec!["a".repeat(257)]).is_err());
        assert!(Ignore::new(vec!["*".to_owned(); 65]).is_err());
        assert!(Ignore::new(vec!["*".to_owned(); 64]).is_ok());
    }
}
