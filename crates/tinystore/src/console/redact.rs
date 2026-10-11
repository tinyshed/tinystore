//! Secrets hidden: the value of a field whose key names one, at any depth of
//! a JSON value, and the password of a URL inside a value. A message's text
//! is not searched.
//!
//! ```text
//! redact ["password"]:  user={"name":"ann","password":"x"}  →  user={"name":"ann","password":"[redacted]"}
//! "see postgres://ann:hunter2@db:5432/app"                  →  "see postgres://ann:[redacted]@db:5432/app"
//! ```

use super::line::quote;

/// What a hidden value becomes, in the store and on the console.
const REDACTED: &str = "\"[redacted]\"";

/// The usual names of secrets, which `redact` takes to hide them. It hides a
/// counter named like one too, `token_count` among them.
pub const SECRETS: &[&str] = &[
    "password",
    "passwd",
    "passphrase",
    "secret",
    "token",
    "credential",
    "credentials",
    "authorization",
    "cookie",
    "api key",
    "private key",
    "secret key",
    "access key",
    "signing key",
    "encryption key",
    "connection string",
    "dsn",
];

/// Hides the values of the fields a logger was told to, and the passwords of
/// URLs inside values unless it was told to keep them.
#[derive(Clone, Debug, Default)]
pub(crate) struct Redactor {
    /// Each name's words written together: `api key` is `apikey`.
    names: Vec<String>,
    urls: bool,
}

impl Redactor {
    pub(crate) fn new<S: AsRef<str>>(names: &[S], keep_url_passwords: bool) -> Redactor {
        let names =
            names.iter().map(|name| words(name.as_ref()).concat()).filter(|joined| !joined.is_empty()).collect();
        Redactor { names, urls: !keep_url_passwords }
    }

    /// Fields with what they must not show hidden.
    pub(crate) fn fields(&self, fields: &mut [(String, String)]) {
        if self.names.is_empty() && !self.urls {
            return;
        }
        for (key, value) in fields.iter_mut() {
            if self.hides(key) {
                REDACTED.clone_into(value);
            } else if let Some(hidden) = self.value(value) {
                *value = hidden;
            }
        }
    }

    /// Whether a key names a secret: some of its words in a row, written
    /// together, are a name's.
    ///
    /// ```text
    /// "password", "api key"  hide  password, DB_PASSWORD, PasswordHash, api_key, apiKey, APIKEY
    /// "token"                hides bot_token, token_count, and not tokens_used
    /// ```
    fn hides(&self, key: &str) -> bool {
        if self.names.is_empty() {
            return false;
        }
        let words = words(key);
        self.names.iter().any(|name| joins_to(&words, name))
    }

    /// A field's JSON with the secrets inside it hidden; none when nothing
    /// was.
    fn value(&self, spelled: &str) -> Option<String> {
        let inner = (!self.names.is_empty()).then(|| self.json(spelled)).flatten();
        let spelled = inner.as_deref().unwrap_or(spelled);
        let urls = self.urls.then(|| hide_url_passwords(spelled)).flatten();
        urls.or(inner)
    }

    /// An object's or an array's JSON with the values of secret keys hidden
    /// at any depth, the order of its keys and the spelling of the rest kept;
    /// none when it hid nothing.
    fn json(&self, spelled: &str) -> Option<String> {
        let trimmed = spelled.trim();
        if !trimmed.starts_with(['{', '[']) {
            return None;
        }
        let mut out = String::with_capacity(spelled.len());
        let mut walk = Walk { text: trimmed.as_bytes(), at: 0 };
        let changed = self.rewrite(&mut walk, &mut out)?;
        (changed && walk.at == trimmed.len()).then_some(out)
    }

    /// Writes the value at the walk's place with the values of secret keys
    /// hidden, compactly, and says whether it hid any; none when it is no
    /// JSON.
    fn rewrite(&self, walk: &mut Walk<'_>, out: &mut String) -> Option<bool> {
        walk.space();
        let open = walk.peek()?;
        if open != b'{' && open != b'[' {
            let value = walk.value()?;
            out.push_str(value);
            return Some(false);
        }
        walk.at += 1;
        out.push(char::from(open));
        let (close, mut changed, mut first) = (if open == b'{' { b'}' } else { b']' }, false, true);
        loop {
            walk.space();
            if walk.peek()? == close {
                walk.at += 1;
                out.push(char::from(close));
                return Some(changed);
            }
            if !first {
                walk.expect(b',')?;
                out.push(',');
                walk.space();
            }
            first = false;
            let hide = if open == b'{' {
                let key = walk.string()?;
                quote(out, &key);
                walk.space();
                walk.expect(b':')?;
                out.push(':');
                self.hides(&key)
            } else {
                false
            };
            if hide {
                walk.space();
                walk.value()?;
                out.push_str(REDACTED);
                changed = true;
            } else {
                changed |= self.rewrite(walk, out)?;
            }
        }
    }
}

/// A key's words as a person reads them, in lower case: split at anything
/// but a letter or a digit, and where a capital begins a word.
///
/// ```text
/// DB_PASSWORD → db password    PasswordHash → password hash    APIKey → api key    signing-key → signing key
/// ```
fn words(key: &str) -> Vec<String> {
    let chars: Vec<char> = key.chars().collect();
    let (mut out, mut start) = (Vec::new(), None);
    for (at, c) in chars.iter().enumerate() {
        if !c.is_alphanumeric() {
            if let Some(from) = start.take() {
                out.push(lower(&chars[from..at]));
            }
            continue;
        }
        if let Some(from) = start
            && starts_word(&chars, at)
        {
            out.push(lower(&chars[from..at]));
            start = Some(at);
        }
        start.get_or_insert(at);
    }
    if let Some(from) = start {
        out.push(lower(&chars[from..]));
    }
    out
}

fn lower(chars: &[char]) -> String {
    chars.iter().collect::<String>().to_lowercase()
}

/// A capital after a small letter or a digit, or a capital before a small
/// letter after capitals: `passwordHash`, `APIKey`.
fn starts_word(chars: &[char], at: usize) -> bool {
    if !chars[at].is_uppercase() {
        return false;
    }
    let before = chars[at - 1];
    let lower_after = chars.get(at + 1).is_some_and(|c| c.is_lowercase());
    before.is_lowercase() || before.is_numeric() || before.is_uppercase() && lower_after
}

/// Whether some words in a row, written together, are `name`.
fn joins_to(words: &[String], name: &str) -> bool {
    (0..words.len()).any(|from| {
        let mut rest = name;
        for word in &words[from..] {
            let Some(after) = rest.strip_prefix(word.as_str()) else { return false };
            rest = after;
            if rest.is_empty() {
                return true;
            }
        }
        false
    })
}

/// A walk over JSON text, which reads its strings and skips its values as
/// they are spelled.
struct Walk<'a> {
    text: &'a [u8],
    at: usize,
}

impl<'a> Walk<'a> {
    fn space(&mut self) {
        while self.text.get(self.at).is_some_and(|byte| byte.is_ascii_whitespace()) {
            self.at += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.text.get(self.at).copied()
    }

    fn expect(&mut self, byte: u8) -> Option<()> {
        (self.peek()? == byte).then(|| self.at += 1)
    }

    /// A string's text, read through its escapes.
    fn string(&mut self) -> Option<String> {
        let start = self.at;
        self.skip_string()?;
        serde_json::from_slice(&self.text[start..self.at]).ok()
    }

    fn skip_string(&mut self) -> Option<()> {
        self.expect(b'"')?;
        loop {
            match self.peek()? {
                b'"' => {
                    self.at += 1;
                    return Some(());
                }
                b'\\' => self.at += 2,
                _ => self.at += 1,
            }
        }
    }

    /// A whole value as it is spelled, nested ones included.
    fn value(&mut self) -> Option<&'a str> {
        let start = self.at;
        match self.peek()? {
            b'"' => self.skip_string()?,
            b'{' | b'[' => {
                let mut depth = 0_usize;
                loop {
                    match self.peek()? {
                        b'"' => {
                            self.skip_string()?;
                            continue;
                        }
                        b'{' | b'[' => depth += 1,
                        b'}' | b']' => {
                            depth -= 1;
                            if depth == 0 {
                                self.at += 1;
                                break;
                            }
                        }
                        _ => {}
                    }
                    self.at += 1;
                }
            }
            _ => {
                while self.peek().is_some_and(|byte| !matches!(byte, b',' | b'}' | b']') && !byte.is_ascii_whitespace())
                {
                    self.at += 1;
                }
            }
        }
        std::str::from_utf8(&self.text[start..self.at]).ok().filter(|value| !value.is_empty())
    }
}

/// The JSON of a value with the password of each URL in its strings hidden,
/// the rest as it was spelled; none when it held none.
fn hide_url_passwords(spelled: &str) -> Option<String> {
    if !spelled.contains("://") && !spelled.contains('\\') {
        return None;
    }
    let bytes = spelled.as_bytes();
    let (mut out, mut written, mut at) = (String::new(), 0, 0);
    while at < bytes.len() {
        if bytes[at] != b'"' {
            at += 1;
            continue;
        }
        let mut walk = Walk { text: bytes, at };
        let text = walk.string()?;
        if let Some(hidden) = hide_url_text_passwords(&text) {
            out.push_str(&spelled[written..at]);
            quote(&mut out, &hidden);
            written = walk.at;
        }
        at = walk.at;
    }
    (written > 0).then(|| out + &spelled[written..])
}

/// Text with the password of each URL in it hidden; none when it held none.
fn hide_url_text_passwords(text: &str) -> Option<String> {
    let (mut out, mut written, mut from) = (String::new(), 0, 0);
    while let Some(found) = text[from..].find("://") {
        let scheme_end = from + found;
        let (password, at, end) = find_password(text, scheme_end + 3);
        from = end;
        if password.is_none() || !ends_in_scheme(&text[..scheme_end]) {
            continue;
        }
        let (password, at) = (password?, at?);
        out.push_str(&text[written..password]);
        out.push_str("[redacted]");
        written = at;
    }
    (written > 0).then(|| out + &text[written..])
}

/// The authority from `start`: where its password begins and the `@` after
/// it, when it has them, and where it ends.
fn find_password(text: &str, start: usize) -> (Option<usize>, Option<usize>, usize) {
    let bytes = text.as_bytes();
    let (mut at, mut colon, mut end) = (None, None, start);
    while end < bytes.len() {
        let c = bytes[end];
        if c == b'/' || c == b'?' || c == b'#' || c <= b' ' {
            break;
        }
        if c == b'@' {
            at = Some(end);
        } else if c == b':' && colon.is_none() {
            colon = Some(end);
        }
        end += 1;
    }
    match (colon, at) {
        (Some(colon), Some(at)) if colon + 1 < at => (Some(colon + 1), Some(at), end),
        _ => (None, None, end),
    }
}

/// Whether text ends in a URL's scheme: letters, digits, `+`, `-` or `.`, a
/// letter first.
fn ends_in_scheme(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut from = bytes.len();
    while from > 0 && (bytes[from - 1].is_ascii_alphanumeric() || matches!(bytes[from - 1], b'+' | b'-' | b'.')) {
        from -= 1;
    }
    from < bytes.len() && bytes[from].is_ascii_alphabetic()
}
