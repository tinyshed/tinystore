//! The schema of protocol/*.wire, and its parser.
//!
//! ```text
//! # a comment above an item documents it
//! message kv.Call {
//!   1 handle: uint
//!   2 under: [key]              # a comment after a field documents the field
//!   4 value: value?
//! }
//! method 0x0102 kv.get(kv.Call) -> kv.Entry
//! method 0x010a kv.list(kv.List) -> download kv.Entry until kv.Page
//! method 0x0209 jobs.work(jobs.Work) -> exchange jobs.Held for jobs.Answer
//! ```

use std::collections::HashSet;
use std::fmt;

#[derive(Debug, Default)]
pub(crate) struct Schema {
    pub(crate) messages: Vec<Message>,
    pub(crate) methods: Vec<Method>,
}

#[derive(Debug)]
pub(crate) struct Message {
    pub(crate) name: String,
    pub(crate) doc: Vec<String>,
    pub(crate) fields: Vec<Field>,
}

#[derive(Debug)]
pub(crate) struct Field {
    pub(crate) number: u64,
    pub(crate) name: String,
    pub(crate) kind: Kind,
    pub(crate) optional: bool,
    pub(crate) doc: Option<String>,
}

/// A field's type, as the schema names it.
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
    List(Box<Kind>),
    /// A map of names, its keys str.
    Names(Box<Kind>),
    Message(String),
}

#[derive(Debug)]
pub(crate) struct Method {
    pub(crate) id: u16,
    pub(crate) name: String,
    pub(crate) request: String,
    pub(crate) answer: Answer,
    pub(crate) access: Access,
}

/// Who may call a method: the word its line starts with. A connection
/// admitted to read only calls `read` alone, and `admin` is an admin's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Access {
    Read,
    Write,
    Admin,
}

/// What a method sends back, by the shape of its stream.
#[derive(Debug)]
pub(crate) enum Answer {
    /// One message, the stream's end.
    Call(String),
    /// Items as DATA, the last DATA its trailer.
    Download { item: String, trailer: String },
    /// One message that ends the stream, or hands the run to the client, whose
    /// last DATA is the same message.
    Handover(String),
    /// Items both ways after an empty RESPONSE: the server's DATA are `out`,
    /// the client's `back`, until each side ends its own.
    Exchange { out: String, back: String },
}

/// Why a schema did not parse, and where.
#[derive(Debug)]
pub(crate) struct Refused {
    pub(crate) file: String,
    pub(crate) line: usize,
    pub(crate) why: String,
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.file, self.line, self.why)
    }
}

impl Schema {
    /// Adds a file's items to the schema.
    pub(crate) fn parse(&mut self, file: &str, text: &str) -> Result<(), Refused> {
        let refused = |line: usize, why: String| Refused { file: file.to_owned(), line, why };
        let mut doc = Vec::new();
        let mut open: Option<Message> = None;
        for (index, raw) in text.lines().enumerate() {
            let line = index + 1;
            let (code, comment) = split_comment(raw);
            if let Some(message) = &mut open {
                if code == "}" {
                    self.messages.push(open.take().expect("a message is open"));
                } else if !code.is_empty() {
                    message.fields.push(field(code, comment).map_err(|why| refused(line, why))?);
                }
                continue;
            }
            match code {
                "" if comment.is_some() && raw.trim_start().starts_with('#') => doc.push(comment.unwrap_or_default()),
                "" => doc.clear(),
                _ if code.starts_with("message ") => {
                    let header = &code["message ".len()..];
                    let (name, body) =
                        header.split_once('{').ok_or_else(|| refused(line, format!("a message without {{: {code}")))?;
                    let message =
                        Message { name: name.trim().to_owned(), doc: std::mem::take(&mut doc), fields: Vec::new() };
                    match body.trim() {
                        "" => open = Some(message),
                        "}" => self.messages.push(message),
                        other => return Err(refused(line, format!("fields on a message's first line: {other}"))),
                    }
                }
                _ if code.starts_with("method ") => {
                    return Err(refused(
                        line,
                        "a method says who may call it, read, write or admin, for `method`".to_owned(),
                    ));
                }
                _ if ["read ", "write ", "admin "].iter().any(|word| code.starts_with(word)) => {
                    self.methods.push(method(code).map_err(|why| refused(line, why))?);
                    doc.clear();
                }
                _ => return Err(refused(line, format!("neither a message nor a method: {code}"))),
            }
        }
        match open {
            Some(message) => Err(refused(text.lines().count(), format!("message {} is never closed", message.name))),
            None => Ok(()),
        }
    }

    /// Refuses a schema that names what it does not declare, declares a
    /// name, a field number or a method twice, numbers a field past what one
    /// byte holds, or nests a value past the profile's eight levels.
    pub(crate) fn check(&self) -> Result<(), String> {
        let mut names = HashSet::new();
        for message in &self.messages {
            if !names.insert(message.name.as_str()) {
                return Err(format!("message {} declared twice", message.name));
            }
        }
        for message in &self.messages {
            let mut numbers = HashSet::new();
            for field in &message.fields {
                if !numbers.insert(field.number) {
                    return Err(format!("{}: field {} twice", message.name, field.number));
                }
                // a positive fixint: a field's number is one byte, which its writer pushes as it is
                if !(1..=127).contains(&field.number) {
                    return Err(format!("{}: field {} is not 1 to 127", message.name, field.number));
                }
                if let Some(missing) = field.kind.message().filter(|name| !names.contains(name)) {
                    return Err(format!("{}.{}: no message {missing}", message.name, field.name));
                }
            }
        }
        if !names.contains("Failure") {
            return Err("no message Failure, which a refused message is".to_owned());
        }
        for message in &self.messages {
            // a message is a value at level 0 and its fields at level 1; a reader
            // that follows the schema meets nothing deeper than it says
            let deepest = 1 + self.below(message, &mut Vec::new())?;
            if deepest > 8 {
                return Err(format!("{} nests a value {deepest} levels down, past the profile's 8", message.name));
            }
        }
        let mut ids = HashSet::new();
        for method in &self.methods {
            if !ids.insert(method.id) {
                return Err(format!("method {:#06x} declared twice", method.id));
            }
            for named in method.messages() {
                if !names.contains(named) {
                    return Err(format!("{}: no message {named}", method.name));
                }
            }
        }
        Ok(())
    }

    /// How many levels below its fields a message's deepest value lies.
    /// `within` holds the messages being measured, one naming itself again
    /// being a cycle.
    fn below<'a>(&'a self, message: &'a Message, within: &mut Vec<&'a str>) -> Result<usize, String> {
        if within.contains(&message.name.as_str()) {
            return Err(format!("{} holds itself, which no depth bounds", message.name));
        }
        within.push(&message.name);
        let mut deepest = 0;
        for field in &message.fields {
            deepest = deepest.max(self.levels(&field.kind, within)?);
        }
        within.pop();
        Ok(deepest)
    }

    /// How many levels below a value of the kind its deepest part lies.
    fn levels<'a>(&'a self, kind: &'a Kind, within: &mut Vec<&'a str>) -> Result<usize, String> {
        match kind {
            Kind::List(item) | Kind::Names(item) => Ok(1 + self.levels(item, within)?),
            Kind::Message(name) => {
                let named = self.message(name);
                let fields = if named.fields.is_empty() { 0 } else { 1 };
                Ok(fields + self.below(named, within)?)
            }
            _ => Ok(0),
        }
    }

    pub(crate) fn message(&self, name: &str) -> &Message {
        self.messages.iter().find(|message| message.name == name).expect("a checked schema declares every name")
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

impl Method {
    fn messages(&self) -> Vec<&str> {
        let mut named = vec![self.request.as_str()];
        match &self.answer {
            Answer::Call(answer) | Answer::Handover(answer) => named.push(answer),
            Answer::Download { item, trailer } => named.extend([item.as_str(), trailer.as_str()]),
            Answer::Exchange { out, back } => named.extend([out.as_str(), back.as_str()]),
        }
        named
    }
}

/// A line's code and its comment, either empty.
fn split_comment(raw: &str) -> (&str, Option<String>) {
    match raw.find('#') {
        Some(at) => (raw[..at].trim(), Some(raw[at + 1..].trim().to_owned())),
        None => (raw.trim(), None),
    }
}

/// `4 value: value?` with its comment.
fn field(code: &str, comment: Option<String>) -> Result<Field, String> {
    let (number, rest) = code.split_once(' ').ok_or_else(|| format!("a field without a number: {code}"))?;
    let number = number.parse().map_err(|_| format!("a field number that is not one: {number}"))?;
    let (name, kind) = rest.split_once(':').ok_or_else(|| format!("a field without a type: {code}"))?;
    let kind = kind.trim();
    let (kind, optional) = match kind.strip_suffix('?') {
        Some(kind) => (kind, true),
        None => (kind, false),
    };
    let doc = comment.filter(|comment| !comment.is_empty());
    Ok(Field { number, name: name.trim().to_owned(), kind: parse_kind(kind)?, optional, doc })
}

fn parse_kind(text: &str) -> Result<Kind, String> {
    if let Some(item) = text.strip_prefix('[').and_then(|text| text.strip_suffix(']')) {
        return Ok(Kind::List(Box::new(parse_kind(item.trim())?)));
    }
    if let Some(item) = text.strip_prefix('{').and_then(|text| text.strip_suffix('}')) {
        return Ok(Kind::Names(Box::new(parse_kind(item.trim())?)));
    }
    Ok(match text {
        "bool" => Kind::Bool,
        "uint" => Kind::Uint,
        "int" => Kind::Int,
        "int64" => Kind::Int64,
        "float" => Kind::Float,
        "str" => Kind::Str,
        "bin" => Kind::Bin,
        "duration" => Kind::Duration,
        "time" => Kind::Time,
        "nanos" => Kind::Nanos,
        "key" => Kind::Key,
        "value" => Kind::Value,
        "cell" => Kind::Cell,
        "json" => Kind::Json,
        name if name.chars().next().is_some_and(char::is_alphabetic) => Kind::Message(name.to_owned()),
        other => return Err(format!("no type {other}")),
    })
}

/// `read 0x010a kv.list(kv.List) -> download kv.Entry until kv.Page`
fn method(code: &str) -> Result<Method, String> {
    let (word, rest) = code.split_once(' ').ok_or_else(|| format!("a method without a number: {code}"))?;
    let access = match word {
        "read" => Access::Read,
        "write" => Access::Write,
        _ => Access::Admin,
    };
    let (id, rest) = rest.trim_start().split_once(' ').ok_or_else(|| format!("a method without a name: {code}"))?;
    let id = id
        .strip_prefix("0x")
        .and_then(|hex| u16::from_str_radix(hex, 16).ok())
        .ok_or_else(|| format!("a method's number is 0x and four hex digits, not {id}"))?;
    let (call, answer) = rest.split_once("->").ok_or_else(|| format!("a method without an answer: {code}"))?;
    let (name, request) = call
        .trim()
        .strip_suffix(')')
        .and_then(|call| call.split_once('('))
        .ok_or_else(|| format!("a method without a request: {code}"))?;
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
    Ok(Method { id, name: name.trim().to_owned(), request: request.trim().to_owned(), answer, access })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_schema_reads_its_messages_fields_and_methods() {
        let mut schema = Schema::default();
        let text = "# a call\nmessage kv.Call {\n  1 handle: uint\n  2 under: [key]  # owners\n  4 value: value?\n}\n\nmessage kv.Page {\n  1 next: key?\n}\nmessage kv.Entry {\n  1 found: bool\n}\nmessage Failure {\n  1 code: str\n}\nread  0x010a kv.list(kv.Call) -> download kv.Entry until kv.Page\n";
        schema.parse("test.wire", text).unwrap();
        schema.check().unwrap();
        let call = schema.message("kv.Call");
        assert_eq!(call.doc, ["a call"]);
        assert_eq!(call.fields[1].kind, Kind::List(Box::new(Kind::Key)));
        assert_eq!(call.fields[1].doc.as_deref(), Some("owners"));
        assert!(call.fields[2].optional);
        assert_eq!((schema.methods[0].id, schema.methods[0].access), (0x010a, Access::Read));
        assert!(
            matches!(&schema.methods[0].answer, Answer::Download { item, trailer } if item == "kv.Entry" && trailer == "kv.Page")
        );
    }

    #[test]
    fn a_method_says_who_may_call_it() {
        let mut schema = Schema::default();
        let refused = schema.parse("test.wire", "method 0x0102 kv.get(kv.Call) -> kv.Entry\n").unwrap_err();
        assert!(refused.to_string().contains("read, write or admin"), "{refused}");
        schema.parse("test.wire", "admin 0x0001 server.stop(Empty) -> Empty\n").unwrap();
        assert_eq!(schema.methods[0].access, Access::Admin);
    }

    #[test]
    fn a_schema_without_the_failure_its_refusals_are_is_refused() {
        let mut schema = Schema::default();
        schema.parse("test.wire", "message Empty {\n}\n").unwrap();
        assert!(schema.check().unwrap_err().contains("no message Failure"));
    }

    #[test]
    fn a_name_the_schema_does_not_declare_is_refused() {
        let mut schema = Schema::default();
        schema.parse("test.wire", "message A {\n  1 b: B\n}\n").unwrap();
        assert!(schema.check().unwrap_err().contains("no message B"));
    }

    #[test]
    fn a_field_past_one_byte_or_a_value_past_eight_levels_is_refused() {
        let checked = |text: &str| {
            let mut schema = Schema::default();
            schema.parse("test.wire", &format!("message Failure {{\n  1 code: str\n}}\n{text}")).unwrap();
            schema.check()
        };
        assert!(checked("message A {\n  127 b: str\n}\n").is_ok());
        assert_eq!(checked("message A {\n  128 b: str\n}\n").unwrap_err(), "A: field 128 is not 1 to 127");
        assert_eq!(checked("message A {\n  0 b: str\n}\n").unwrap_err(), "A: field 0 is not 1 to 127");

        // A's fields are at level 1 and each list goes one down: seven lists put a cell at 8, eight at 9
        assert!(checked("message A {\n  1 b: [[[[[[[cell]]]]]]]\n}\n").is_ok());
        let deep = checked("message A {\n  1 b: [[[[[[[[cell]]]]]]]]\n}\n").unwrap_err();
        assert_eq!(deep, "A nests a value 9 levels down, past the profile's 8");
        // a message held is a level too, its fields one below it
        let held = checked("message A {\n  1 b: [[[[[[B]]]]]]\n}\nmessage B {\n  1 c: [cell]\n}\n").unwrap_err();
        assert_eq!(held, "A nests a value 9 levels down, past the profile's 8");
    }
}
