//! The `.wire` language: a file as it is written, line for line, and its one
//! parser. The schema is built and checked from what it reads and `format`
//! prints it back laid out, so that nothing else reads the text.
//! protocol/README.md is the language's reference.
//!
//! ```text
//! # a comment above a message or a method documents it
//! message kv.Call {
//!   1 handle: uint
//!   2 under: [key]          # a comment after a field documents the field
//! }
//!
//! read  0x0102 kv.get(kv.Call)  -> kv.Entry
//! write 0x010a kv.list(kv.List) -> download kv.Entry until kv.Page
//! ```

use std::fmt;

/// A file's lines, each with its number for the error that names it.
pub(crate) type File = Vec<(usize, Line)>;

#[derive(Debug, PartialEq)]
pub(crate) enum Line {
    Blank,
    /// A comment alone on its line, past its `#` and one space: the doc of
    /// the message or the method below it, and inside a message a note.
    Comment(String),
    /// `message kv.Call {`, or a message without fields whole: `message Empty {}`.
    Message {
        name: String,
        empty: bool,
    },
    /// The `}` that ends a message.
    End,
    Field(Field),
    Method(Method),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Field {
    pub(crate) number: u64,
    pub(crate) name: String,
    pub(crate) kind: Kind,
    /// Its type ends with `?`.
    pub(crate) optional: bool,
    /// The comment after it.
    pub(crate) doc: Option<String>,
}

/// A field's type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Bool,
    Uint,
    Int,
    Int64,
    Float,
    Str,
    Bin,
    /// Milliseconds, as a uint.
    Duration,
    /// Unix milliseconds, as an int.
    Time,
    /// Unix nanoseconds, as an int.
    Nanos,
    /// Text: a str, a bin that is not UTF-8, or an integer's decimal spelling.
    Key,
    /// A kv row: nil, an integer or bin.
    Value,
    /// A value as SQLite keeps it: nil, an integer, a float, a str or bin.
    Cell,
    /// JSON's text, as a str.
    Json,
    /// `[T]`
    List(Box<Kind>),
    /// `{T}`: a map of names, its keys str.
    Names(Box<Kind>),
    Message(String),
}

/// Every type the language has built in, by its word; any other name is a
/// message's.
pub(crate) static BUILTINS: [(&str, Kind); 14] = [
    ("bool", Kind::Bool),
    ("uint", Kind::Uint),
    ("int", Kind::Int),
    ("int64", Kind::Int64),
    ("float", Kind::Float),
    ("str", Kind::Str),
    ("bin", Kind::Bin),
    ("duration", Kind::Duration),
    ("time", Kind::Time),
    ("nanos", Kind::Nanos),
    ("key", Kind::Key),
    ("value", Kind::Value),
    ("cell", Kind::Cell),
    ("json", Kind::Json),
];

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Method {
    pub(crate) access: Access,
    pub(crate) id: u16,
    pub(crate) name: String,
    pub(crate) request: String,
    pub(crate) answer: Answer,
}

/// Who may call a method, the word its line starts with: a connection
/// admitted to read only calls `read` alone, and `admin` is an admin's. A
/// method has no default, so that none is declared without saying.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Access {
    Read,
    Write,
    Admin,
}

/// Every word a method's line may start with.
pub(crate) const ACCESS: [(&str, Access); 3] =
    [("read", Access::Read), ("write", Access::Write), ("admin", Access::Admin)];

/// What a method sends back, by the shape of its stream.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Answer {
    /// `T`: one message, the stream's end.
    Call(String),
    /// `download T until U`: items as DATA, the last DATA its trailer.
    Download { item: String, trailer: String },
    /// `handover T`: one message that ends the stream, or hands the run to
    /// the client, whose last DATA is the same message.
    Handover(String),
    /// `exchange T for U`: items both ways after an empty RESPONSE, the
    /// server's DATA `out` and the client's `back`, until each side ends its
    /// own.
    Exchange { out: String, back: String },
}

/// A file's lines, or the number of the first that is not the language's
/// and why.
pub(crate) fn parse(text: &str) -> Result<File, (usize, String)> {
    let mut lines = Vec::new();
    let mut inside = None;
    for (index, raw) in text.lines().enumerate() {
        let number = index + 1;
        let line = line(raw.trim(), inside.is_some()).map_err(|why| (number, why))?;
        match &line {
            Line::Message { name, empty: false } => inside = Some(name.clone()),
            Line::End => inside = None,
            _ => {}
        }
        lines.push((number, line));
    }
    match inside {
        Some(name) => Err((lines.len(), format!("message {name} is never closed"))),
        None => Ok(lines),
    }
}

/// What one line is, inside a message's braces or out of them.
fn line(text: &str, inside: bool) -> Result<Line, String> {
    if text.is_empty() {
        return Ok(Line::Blank);
    }
    if let Some(comment) = text.strip_prefix('#') {
        return Ok(Line::Comment(comment.strip_prefix(' ').unwrap_or(comment).to_owned()));
    }
    let (code, comment) = match text.split_once('#') {
        Some((code, comment)) => (code.trim(), Some(comment.trim())),
        None => (text, None),
    };
    if inside && code != "}" {
        return field(code, comment).map(Line::Field);
    }
    if comment.is_some() {
        return Err("only a field has a comment after it; a message's or a method's goes above".to_owned());
    }
    if inside {
        return Ok(Line::End);
    }
    if let Some(header) = code.strip_prefix("message ") {
        return message(header);
    }
    if code.starts_with("method ") {
        return Err("a method says who may call it, read, write or admin, for `method`".to_owned());
    }
    let (word, rest) = code.split_once(' ').unwrap_or((code, ""));
    match ACCESS.iter().find(|(access, _)| *access == word) {
        Some((_, access)) => method(*access, rest).map(Line::Method),
        None => Err(format!("neither a message nor a method: {code}")),
    }
}

/// `kv.Call {`, or `Empty {}`.
fn message(header: &str) -> Result<Line, String> {
    let (name, body) = header.split_once('{').ok_or_else(|| format!("a message without {{: message {header}"))?;
    let name = name.trim().to_owned();
    match body.trim() {
        "" => Ok(Line::Message { name, empty: false }),
        "}" => Ok(Line::Message { name, empty: true }),
        other => Err(format!("fields on a message's first line: {other}")),
    }
}

/// `4 value: value?`
fn field(code: &str, comment: Option<&str>) -> Result<Field, String> {
    let (number, rest) = code.split_once(' ').ok_or_else(|| format!("a field without a number: {code}"))?;
    let number = number.parse().map_err(|_| format!("a field number that is not one: {number}"))?;
    let (name, written) = rest.split_once(':').ok_or_else(|| format!("a field without a type: {code}"))?;
    let written: String = written.split_whitespace().collect();
    let (written, optional) = match written.strip_suffix('?') {
        Some(written) => (written, true),
        None => (written.as_str(), false),
    };
    let doc = comment.filter(|comment| !comment.is_empty()).map(str::to_owned);
    Ok(Field { number, name: name.trim().to_owned(), kind: kind(written)?, optional, doc })
}

/// `[key]`, `{str}`, `uint` or a message's name.
fn kind(text: &str) -> Result<Kind, String> {
    if let Some(item) = text.strip_prefix('[').and_then(|text| text.strip_suffix(']')) {
        return Ok(Kind::List(Box::new(kind(item)?)));
    }
    if let Some(item) = text.strip_prefix('{').and_then(|text| text.strip_suffix('}')) {
        return Ok(Kind::Names(Box::new(kind(item)?)));
    }
    if let Some((_, builtin)) = BUILTINS.iter().find(|(word, _)| *word == text) {
        return Ok(builtin.clone());
    }
    match text.chars().next().is_some_and(char::is_alphabetic) {
        true => Ok(Kind::Message(text.to_owned())),
        false => Err(format!("no type {text}")),
    }
}

/// `0x010a kv.list(kv.List) -> download kv.Entry until kv.Page`, past its word.
fn method(access: Access, rest: &str) -> Result<Method, String> {
    let (id, rest) = rest.trim_start().split_once(' ').ok_or_else(|| format!("a method without a name: {rest}"))?;
    let id = id
        .strip_prefix("0x")
        .filter(|hex| hex.len() == 4)
        .and_then(|hex| u16::from_str_radix(hex, 16).ok())
        .ok_or_else(|| format!("a method's number is 0x and four hex digits, not {id}"))?;
    let (call, answer) = rest.split_once("->").ok_or_else(|| format!("a method without an answer: {rest}"))?;
    let call: String = call.split_whitespace().collect();
    let (name, request) = call
        .strip_suffix(')')
        .and_then(|call| call.split_once('('))
        .ok_or_else(|| format!("a method without a request: {call}"))?;
    let words: Vec<&str> = answer.split_whitespace().collect();
    let answer = match words.as_slice() {
        [answer] => Answer::Call((*answer).to_owned()),
        ["download", item, "until", trailer] => {
            Answer::Download { item: (*item).to_owned(), trailer: (*trailer).to_owned() }
        }
        ["handover", answer] => Answer::Handover((*answer).to_owned()),
        ["exchange", out, "for", back] => Answer::Exchange { out: (*out).to_owned(), back: (*back).to_owned() },
        _ => return Err(format!("an answer that is a message, a download, a handover or an exchange: {answer}")),
    };
    Ok(Method { access, id, name: name.to_owned(), request: request.to_owned(), answer })
}

impl Access {
    pub(crate) fn word(self) -> &'static str {
        ACCESS.iter().find(|(_, access)| *access == self).map_or("", |(word, _)| word)
    }
}

impl Answer {
    /// The word a shape is known by: its first, and `call` for one message.
    pub(crate) fn shape(&self) -> &'static str {
        match self {
            Answer::Call(_) => "call",
            Answer::Download { .. } => "download",
            Answer::Handover(_) => "handover",
            Answer::Exchange { .. } => "exchange",
        }
    }

    /// The messages it names.
    pub(crate) fn messages(&self) -> Vec<&str> {
        match self {
            Answer::Call(answer) | Answer::Handover(answer) => vec![answer],
            Answer::Download { item, trailer } => vec![item, trailer],
            Answer::Exchange { out, back } => vec![out, back],
        }
    }
}

impl fmt::Display for Answer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Answer::Call(answer) => f.write_str(answer),
            Answer::Download { item, trailer } => write!(f, "download {item} until {trailer}"),
            Answer::Handover(answer) => write!(f, "handover {answer}"),
            Answer::Exchange { out, back } => write!(f, "exchange {out} for {back}"),
        }
    }
}

impl Kind {
    /// The message this kind names, inside a list or a map too.
    pub(crate) fn message(&self) -> Option<&str> {
        match self {
            Kind::Message(name) => Some(name),
            Kind::List(item) | Kind::Names(item) => item.message(),
            _ => None,
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Kind::List(item) => write!(f, "[{item}]"),
            Kind::Names(item) => write!(f, "{{{item}}}"),
            Kind::Message(name) => f.write_str(name),
            builtin => f.write_str(BUILTINS.iter().find(|(_, kind)| kind == builtin).map_or("", |(word, _)| word)),
        }
    }
}

/// The words an answer's shapes are written with, taken from how each prints.
#[cfg(test)]
pub(crate) fn shape_words() -> Vec<String> {
    let (a, b) = ("A".to_owned(), "B".to_owned());
    let shaped = [
        Answer::Download { item: a.clone(), trailer: b.clone() },
        Answer::Handover(a.clone()),
        Answer::Exchange { out: a, back: b },
    ];
    let printed: Vec<String> = shaped.iter().map(ToString::to_string).collect();
    let words = printed.iter().flat_map(|printed| printed.split(' '));
    words.filter(|word| !matches!(*word, "A" | "B")).map(str::to_owned).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_is_what_its_first_word_says() {
        assert_eq!(line("", false), Ok(Line::Blank));
        assert_eq!(line("#  why", false), Ok(Line::Comment(" why".to_owned())));
        assert_eq!(line("message Empty {}", false), Ok(Line::Message { name: "Empty".to_owned(), empty: true }));
        assert_eq!(line("}", true), Ok(Line::End));
        let field = Field {
            number: 2,
            name: "under".to_owned(),
            kind: Kind::List(Box::new(Kind::Key)),
            optional: true,
            doc: Some("owners".to_owned()),
        };
        assert_eq!(line("2 under:  [ key ]?   # owners", true), Ok(Line::Field(field)));
        let method = Method {
            access: Access::Read,
            id: 0x010a,
            name: "kv.list".to_owned(),
            request: "kv.List".to_owned(),
            answer: Answer::Download { item: "kv.Entry".to_owned(), trailer: "kv.Page".to_owned() },
        };
        let written = "read 0x010a kv.list( kv.List ) ->  download kv.Entry until kv.Page";
        assert_eq!(line(written, false), Ok(Line::Method(method)));
    }

    #[test]
    fn a_line_that_is_not_the_languages_says_why() {
        let refused = |text: &str, inside: bool| line(text, inside).unwrap_err();
        assert!(refused("method 0x0102 kv.get(kv.Call) -> kv.Entry", false).contains("read, write or admin"));
        assert!(refused("query 0x0102 kv.get(kv.Call) -> kv.Entry", false).contains("neither"));
        assert!(refused("read 0x102 kv.get(kv.Call) -> kv.Entry", false).contains("four hex digits"));
        assert!(refused("read 0x0102 kv.get(kv.Call) -> stream kv.Entry", false).contains("an answer that is"));
        assert!(refused("read 0x0102 kv.get(kv.Call) -> kv.Entry # gets", false).contains("goes above"));
        assert!(refused("message Empty {} # nothing", false).contains("goes above"));
        assert!(refused("x handle: uint", true).contains("a field number"));
        assert!(refused("1 handle: 9", true).contains("no type 9"));
    }

    #[test]
    fn a_message_left_open_names_itself() {
        assert_eq!(
            parse("message kv.Call {\n  1 handle: uint\n").unwrap_err(),
            (2, "message kv.Call is never closed".to_owned())
        );
    }

    #[test]
    fn a_type_and_an_answer_print_as_they_are_written() {
        for written in ["uint", "[key]", "{[cell]}", "kv.Call"] {
            assert_eq!(kind(written).unwrap().to_string(), written);
        }
        for (word, builtin) in &BUILTINS {
            assert_eq!((kind(word).as_ref(), builtin.to_string().as_str()), (Ok(builtin), *word));
        }
        assert_eq!(shape_words(), ["download", "until", "handover", "exchange", "for"]);
    }
}
