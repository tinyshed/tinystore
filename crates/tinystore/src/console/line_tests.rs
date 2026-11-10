use jiff::tz::TimeZone;

use super::{CYAN, DIM, Line, Look, MAGENTA, RESET, SOURCE, Time, pretty, quoted};

/// Each character a pretty line shows as its escape when a program logged it:
/// every control character but a tab, and DEL.
fn escaped_characters() -> impl Iterator<Item = char> {
    ('\0'..' ').filter(|c| *c != '\t').chain(['\x7f'])
}

/// A character's escape as a value's JSON spells it, and DEL's, which JSON
/// leaves as it is.
fn escape_of(c: char) -> String {
    if c == '\x7f' { "\\u007f".to_owned() } else { quoted(&c.to_string()).trim_matches('"').to_owned() }
}

/// An event named and carrying a message of two lines, with a source and a
/// value of two lines under a key, each holding `c`: what a program logged
/// in every place a pretty line shows it.
fn holding(c: char) -> Line {
    let text = format!("a{c}b");
    let file = quoted(&format!("/srv/{text}/main.go"));
    Line {
        stream: "api".to_owned(),
        name: format!("signup{c}"),
        message: Some(format!("{text}\n{text}")),
        fields: vec![
            (SOURCE.to_owned(), format!(r#"{{"file":{file},"line":3}}"#)),
            (format!("out{c}"), quoted(&format!("{text}\n{text}"))),
        ],
        ..Line::default()
    }
}

#[test]
fn no_control_character_a_program_logged_reaches_the_terminal() {
    let look = Look { color: true, zone: TimeZone::UTC, time: Time::Off, hide_stream: false };
    for c in escaped_characters() {
        let printed = pretty(&holding(c), &look);
        // the console's own colours are the only escape sequences its line holds
        let unpainted = [RESET, DIM, MAGENTA, CYAN].iter().fold(printed.clone(), |text, hue| text.replace(hue, ""));
        let lines: Vec<&str> = unpainted.strip_suffix('\n').unwrap_or_default().split('\n').collect();
        let escape = escape_of(c);
        // a newline breaks the message, and is an escape in the name, a key and a source
        let (first, count) = if c == '\n' { ("a".to_owned(), 8) } else { (format!("a{escape}b"), 4) };
        assert_eq!(lines[0], format!("EVENT api  signup{escape}  {first}  source=a{escape}b/main.go:3"), "{c:?}");
        assert_eq!(lines.len(), count, "{c:?}: every line of the message and the value: {printed:?}");
        assert_eq!(lines.iter().filter(|shown| shown.starts_with(&format!("    out{escape}: a"))).count(), 1, "{c:?}");
        for shown in &lines {
            let raw = shown.chars().any(|held| (held < ' ' && held != '\t') || held == '\x7f');
            assert!(!raw, "{c:?} reached the terminal: {printed:?}");
        }
        for shown in &lines[1..] {
            assert!(shown.starts_with("    "), "{c:?} began a line of its own: {printed:?}");
        }
    }
}
