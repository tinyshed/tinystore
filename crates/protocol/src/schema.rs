//! The schema of protocol/*.wire: every file's messages and methods together,
//! built from the lines `syntax` reads, and what makes them one protocol.

use std::collections::HashSet;
use std::mem;

use super::syntax::{self, Line};
pub(crate) use super::syntax::{Access, Answer, Field, Kind};

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

/// A method's line, and the comment above it.
#[derive(Debug)]
pub(crate) struct Method {
    pub(crate) doc: Vec<String>,
    pub(crate) access: Access,
    pub(crate) id: u16,
    pub(crate) name: String,
    pub(crate) request: String,
    pub(crate) answer: Answer,
}

impl Schema {
    /// Adds a file's messages and methods. The comment lines right above one
    /// are its doc; a blank line ends them, and a comment inside a message
    /// is a note that documents nothing.
    pub(crate) fn add(&mut self, lines: &syntax::File) {
        let mut doc = Vec::new();
        let mut open: Option<Message> = None;
        for (_, line) in lines {
            match line {
                Line::Blank => doc.clear(),
                Line::Comment(text) if open.is_none() => doc.push(text.trim().to_owned()),
                Line::Comment(_) => {}
                Line::Message { name, empty } => {
                    let message = Message { name: name.clone(), doc: mem::take(&mut doc), fields: Vec::new() };
                    match empty {
                        true => self.messages.push(message),
                        false => open = Some(message),
                    }
                }
                Line::End => self.messages.extend(open.take()),
                Line::Field(field) => {
                    if let Some(message) = &mut open {
                        message.fields.push(field.clone());
                    }
                }
                Line::Method(method) => self.methods.push(Method::documented(method, mem::take(&mut doc))),
            }
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

impl Method {
    fn documented(method: &syntax::Method, doc: Vec<String>) -> Method {
        let syntax::Method { access, id, name, request, answer } = method.clone();
        Method { doc, access, id, name, request, answer }
    }

    fn messages(&self) -> Vec<&str> {
        let mut named = vec![self.request.as_str()];
        named.extend(self.answer.messages());
        named
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema(text: &str) -> Schema {
        let mut schema = Schema::default();
        schema.add(&syntax::parse(text).unwrap());
        schema
    }

    #[test]
    fn a_schema_reads_its_messages_fields_and_methods() {
        let text = [
            "# a call",
            "message kv.Call {",
            "  1 handle: uint",
            "  2 under: [key]  # owners",
            "  4 value: value?",
            "}",
            "",
            "message kv.Page {",
            "  1 next: key?",
            "}",
            "message kv.Entry {",
            "  1 found: bool",
            "}",
            "message Failure {",
            "  1 code: str",
            "}",
            "",
            "# not its doc",
            "",
            "# lists a branch",
            "read  0x010a kv.list(kv.Call) -> download kv.Entry until kv.Page",
        ];
        let schema = schema(&text.join("\n"));
        schema.check().unwrap();
        let call = schema.message("kv.Call");
        assert_eq!(call.doc, ["a call"]);
        assert_eq!(call.fields[1].kind, Kind::List(Box::new(Kind::Key)));
        assert_eq!(call.fields[1].doc.as_deref(), Some("owners"));
        assert!(call.fields[2].optional);
        assert_eq!((schema.methods[0].id, schema.methods[0].access), (0x010a, Access::Read));
        assert_eq!(schema.methods[0].doc, ["lists a branch"]);
        assert_eq!(schema.methods[0].answer.to_string(), "download kv.Entry until kv.Page");
    }

    #[test]
    fn a_schema_without_the_failure_its_refusals_are_is_refused() {
        let schema = schema("message Empty {\n}\n");
        assert!(schema.check().unwrap_err().contains("no message Failure"));
    }

    #[test]
    fn a_name_the_schema_does_not_declare_is_refused() {
        let schema = schema("message A {\n  1 b: B\n}\n");
        assert!(schema.check().unwrap_err().contains("no message B"));
    }

    #[test]
    fn a_field_past_one_byte_or_a_value_past_eight_levels_is_refused() {
        let checked = |text: &str| schema(&format!("message Failure {{\n  1 code: str\n}}\n{text}")).check();
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
