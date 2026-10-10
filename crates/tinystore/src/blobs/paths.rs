//! A file's path: segments between `/`, a folder's and then the file's own, so
//! that a folder is one range of the table.
//!
//! ```text
//! folder("users").folder(42), "photos/1.jpg" → users/42/photos/1.jpg
//! "photos//1.jpg", "../x", "a/./b"            → invalid
//! ```

use crate::{Error, Result};

/// The longest path, its folders included.
pub(crate) const MAX_PATH: usize = 1 << 10;

/// The most segments a path has, its folders' included.
pub(crate) const MAX_SEGMENTS: usize = 16;

/// A folder: the segments above a file's own, as the text its paths start with.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Folder {
    /// `users/42/`, or nothing for the files themselves.
    prefix: String,
    depth: usize,
}

impl Folder {
    /// The folder in this one that `segment` names.
    pub(crate) fn folder(&self, segment: &str) -> Result<Folder> {
        if segment.contains('/') {
            return Err(Error::invalid(format!("a folder is one segment, and {segment:?} holds a /")));
        }
        check_segment(segment)?;
        let folder = Folder { prefix: format!("{}{segment}/", self.prefix), depth: self.depth + 1 };
        check(folder.prefix.len(), folder.depth)?;
        Ok(folder)
    }

    /// The whole path of `path` in this folder.
    pub(crate) fn path(&self, path: &str) -> Result<String> {
        let mut segments = self.depth;
        for segment in path.split('/') {
            check_segment(segment)?;
            segments += 1;
        }
        let whole = format!("{}{path}", self.prefix);
        check(whole.len(), segments)?;
        Ok(whole)
    }

    /// A path of this folder as the folder names it.
    pub(crate) fn relative<'p>(&self, path: &'p str) -> &'p str {
        path.strip_prefix(self.prefix.as_str()).unwrap_or(path)
    }

    pub(crate) fn prefix(&self) -> &str {
        &self.prefix
    }

    /// The paths of this folder that start with `text`: from the first of
    /// them to the first text past them all, or to the end.
    pub(crate) fn range(&self, text: &str) -> Result<(String, Option<String>)> {
        if text.len() > MAX_PATH || text.chars().any(char::is_control) {
            return Err(Error::invalid(format!("a prefix no path starts with: {text:?}")));
        }
        let from = format!("{}{text}", self.prefix);
        let to = past(&from);
        Ok((from, to))
    }
}

/// Refuses what a segment cannot be: empty, `.` or `..`, or holding a control
/// character, NUL among them.
pub(crate) fn check_segment(segment: &str) -> Result<()> {
    match segment {
        "" => Err(Error::invalid("an empty segment")),
        "." | ".." => Err(Error::invalid(format!("a segment {segment:?}"))),
        _ if segment.chars().any(char::is_control) => {
            Err(Error::invalid(format!("a segment holding a control character: {segment:?}")))
        }
        _ => Ok(()),
    }
}

fn check(length: usize, segments: usize) -> Result<()> {
    if segments > MAX_SEGMENTS {
        return Err(Error::invalid(format!("a path of {segments} segments, over {MAX_SEGMENTS}")));
    }
    if length > MAX_PATH {
        return Err(Error::invalid(format!("a path of {length} bytes, over 1 KiB")));
    }
    Ok(())
}

/// The first text past every text that starts with `prefix`: its last
/// character raised by one, or none when no text is past them. Text compares
/// as UTF-8 bytes, which keep the characters' order.
///
/// ```text
/// "users/4/" → "users/40", so "users/42/…" lies past it
/// ""         → none: every path
/// ```
pub(crate) fn past(prefix: &str) -> Option<String> {
    let mut chars: Vec<char> = prefix.chars().collect();
    while let Some(last) = chars.pop() {
        let next = u32::from(last) + 1;
        // the code points after U+D7FF are surrogates, which no text holds
        if let Some(next) = char::from_u32(next).or_else(|| (next == 0xd800).then_some('\u{e000}')) {
            chars.push(next);
            return Some(chars.into_iter().collect());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_is_whole_segments_and_a_path_is_its_folders_and_its_own() {
        let folder = Folder::default().folder("users").unwrap().folder("42").unwrap();
        assert_eq!(folder.path("photos/1.jpg").unwrap(), "users/42/photos/1.jpg");
        assert_eq!(folder.relative("users/42/photos/1.jpg"), "photos/1.jpg");
        for bad in ["", ".", "..", "a\u{0}b", "4/2"] {
            assert!(Folder::default().folder(bad).is_err(), "{bad:?} names a folder");
        }
        for bad in ["photos//1.jpg", "../x", "a/./b", "/a", "a/"] {
            assert!(folder.path(bad).is_err(), "{bad:?} is a path");
        }
    }

    #[test]
    fn a_path_past_its_bounds_is_refused() {
        let deep = (0..15).fold(Folder::default(), |folder, n| folder.folder(&n.to_string()).unwrap());
        assert!(deep.path("last").is_ok());
        assert!(deep.path("one/more").is_err());
        assert!(Folder::default().path(&"a".repeat(MAX_PATH + 1)).is_err());
    }

    #[test]
    fn the_text_past_a_prefix_bounds_exactly_the_texts_that_start_with_it() {
        assert_eq!(past("users/4/").as_deref(), Some("users/40"));
        assert_eq!(past(""), None);
        assert_eq!(past("a\u{d7ff}").as_deref(), Some("a\u{e000}"));
        assert_eq!(past("a\u{10ffff}").as_deref(), Some("b"));
        let to = past("users/4/").unwrap();
        for (path, inside) in [("users/4/a", true), ("users/42/a", false), ("users/40", false), ("users/4/", true)] {
            assert_eq!(path >= "users/4/" && path < to.as_str(), inside, "{path}");
        }
    }
}
