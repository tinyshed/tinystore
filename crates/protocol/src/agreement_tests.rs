//! What is written about the language outside its parser, held to it: the
//! editor's grammar and snippets in editors/wire, and the reference in
//! protocol/README.md. A word the parser reads that one of them leaves out,
//! or an example the parser refuses, fails here.

use std::fs;
use std::path::Path;

use serde_json::Value;

use super::format;
use super::syntax::{self, ACCESS, BUILTINS};

fn text_of(file: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(file);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("{file}: {error}"))
}

fn json_of(file: &str) -> Value {
    serde_json::from_str(&text_of(file)).unwrap_or_else(|error| panic!("{file}: {error}"))
}

fn builtins() -> Vec<String> {
    BUILTINS.iter().map(|(word, _)| (*word).to_owned()).collect()
}

fn access() -> Vec<String> {
    ACCESS.iter().map(|(word, _)| (*word).to_owned()).collect()
}

fn sorted(mut words: Vec<String>) -> Vec<String> {
    words.sort();
    words
}

/// The words a grammar rule matches: those between its first `(` and `)`.
fn matched(rule: &Value) -> Vec<String> {
    let pattern = rule["match"].as_str().expect("a rule that matches");
    let (_, group) = pattern.split_once('(').expect("a group of words");
    let (words, _) = group.split_once(')').expect("a group that ends");
    sorted(words.trim_start_matches("?:").split('|').map(str::to_owned).collect())
}

/// A snippet's body with every placeholder at its default, `${1:name}` as
/// `name` and `${2|a,b|}` as `a`, and each list of choices it offers.
fn filled(body: &str) -> (String, Vec<Vec<String>>) {
    let mut text = String::new();
    let mut choices = Vec::new();
    let mut rest = body;
    while let Some((before, placeholder)) = rest.split_once("${") {
        let (inner, after) = placeholder.split_once('}').expect("a placeholder that ends");
        let inner = inner.trim_start_matches(|char: char| char.is_ascii_digit());
        text.push_str(before);
        match inner.strip_prefix('|') {
            Some(list) => {
                let list: Vec<String> = list.trim_end_matches('|').split(',').map(str::to_owned).collect();
                text.push_str(&list[0]);
                choices.push(list);
            }
            None => text.push_str(inner.strip_prefix(':').unwrap_or(inner)),
        }
        rest = after;
    }
    text.push_str(rest);
    (text, choices)
}

#[test]
fn the_grammar_colours_the_words_the_parser_reads() {
    let grammar = json_of("editors/wire/syntaxes/wire.tmLanguage.json");
    let rules = &grammar["repository"];
    assert_eq!(matched(&rules["builtin"]), sorted(builtins()));

    let method = rules["method"]["patterns"].as_array().expect("a method's rules");
    let coloured =
        |scope: &str| method.iter().find(|rule| rule.to_string().contains(scope)).expect("a rule of the scope");
    assert_eq!(matched(coloured("keyword.declaration.method.wire")), sorted(access()));
    assert_eq!(matched(coloured("keyword.control.wire")), sorted(syntax::shape_words()));
}

#[test]
fn every_snippet_is_what_the_parser_reads_and_offers_every_type() {
    let snippets = json_of("editors/wire/snippets/wire.json");
    for (name, snippet) in snippets.as_object().expect("snippets by name") {
        let body = match &snippet["body"] {
            Value::Array(lines) => lines.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("\n"),
            body => body.as_str().expect("a body of text").to_owned(),
        };
        let (text, choices) = filled(&body);
        let alone = syntax::parse(&text);
        let in_a_message = syntax::parse(&format!("message M {{\n{text}\n}}"));
        assert!(alone.is_ok() || in_a_message.is_ok(), "{name}: {text}: {alone:?}");
        for offered in choices {
            assert_eq!(offered, builtins(), "{name}");
        }
    }
}

#[test]
fn the_reference_shows_every_word_and_its_examples_are_laid_out() {
    let reference = text_of("protocol/README.md");
    // every second piece between backticks is code, a fenced example among them
    let code: Vec<&str> = reference.split('`').skip(1).step_by(2).collect();
    let shown: Vec<&str> = code.iter().flat_map(|code| code.split(|char: char| !char.is_alphanumeric())).collect();
    for word in builtins().into_iter().chain(access()).chain(syntax::shape_words()) {
        assert!(shown.contains(&word.as_str()), "protocol/README.md shows no `{word}`");
    }
    let examples: Vec<&str> =
        reference.split("```wire\n").skip(1).filter_map(|rest| rest.split("```").next()).collect();
    assert!(!examples.is_empty(), "protocol/README.md shows no example");
    for example in examples {
        let lines = syntax::parse(example).unwrap_or_else(|(line, why)| panic!("line {line}: {why}\n{example}"));
        assert_eq!(format::print(&lines), example, "an example as `just protocol` would lay it out");
    }
}
