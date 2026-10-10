//! The `.wire` files' one layout, which `just protocol` writes and CI checks,
//! so that a schema reads as a table: a run of methods with their calls in
//! one column and their answers in the next, and a file's field comments in
//! one column.
//!
//! ```text
//! read  0x0102 kv.get(kv.Call)        -> kv.Entry
//! write 0x0104 kv.set(kv.Call)        -> kv.Written
//!
//! message kv.Call {
//!   1 handle: uint
//!   2 under: [key]          # the branch's owners, outermost first
//!   6 expiresAt: time?      # the expiry a write gives, as a time
//! }
//! ```

/// A schema file in its layout; what is laid out already comes back as it was.
pub(crate) fn format(text: &str) -> String {
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    let column = comment_column(&lines);
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while at < lines.len() {
        let run = lines[at..].iter().take_while(|line| method(line).is_some()).count();
        if run > 0 {
            write_methods(&mut out, &lines[at..at + run]);
            at += run;
            continue;
        }
        match field(lines[at]) {
            Some((code, Some(comment))) => out.push_str(&format!("{code:<column$}# {comment}\n")),
            Some((code, None)) => out.push_str(&format!("{code}\n")),
            None => out.push_str(&format!("{}\n", lines[at])),
        }
        at += 1;
    }
    out
}

/// A run of methods, their calls padded so that every `->` lines up.
fn write_methods(out: &mut String, run: &[&str]) {
    let methods: Vec<_> = run.iter().filter_map(|line| method(line)).collect();
    let width = methods.iter().map(|method| method.call.len()).max().unwrap_or(0);
    for Method { access, id, call, answer } in methods {
        out.push_str(&format!("{access:<5} {id} {call:<width$} -> {answer}\n"));
    }
}

/// The column a file's field comments start at: two past its longest
/// commented field.
fn comment_column(lines: &[&str]) -> usize {
    let longest = lines.iter().filter_map(|line| field(line)).filter(|(_, comment)| comment.is_some());
    longest.map(|(code, _)| code.len()).max().unwrap_or(0) + 2
}

struct Method<'a> {
    access: &'a str,
    id: &'a str,
    call: String,
    answer: String,
}

/// A method's line in its parts: `read 0x0102 kv.get(kv.Call) -> kv.Entry`.
fn method(line: &str) -> Option<Method<'_>> {
    let (access, rest) = line.split_once(' ')?;
    if !matches!(access, "read" | "write" | "admin") {
        return None;
    }
    let (id, rest) = rest.trim_start().split_once(' ')?;
    let (call, answer) = rest.split_once("->")?;
    let call = call.split_whitespace().collect::<String>();
    let answer = answer.split_whitespace().collect::<Vec<_>>().join(" ");
    Some(Method { access, id, call, answer })
}

/// A field's line, its spaces made single, and its comment apart.
fn field(line: &str) -> Option<(String, Option<&str>)> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    let (code, comment) = match trimmed.split_once('#') {
        Some((code, comment)) => (code, Some(comment.trim())),
        None => (trimmed, None),
    };
    Some((format!("  {}", code.split_whitespace().collect::<Vec<_>>().join(" ")), comment))
}

#[cfg(test)]
mod tests {
    use super::format;

    #[test]
    fn methods_line_up_their_answers_and_fields_their_comments() {
        let text = "message kv.Call {\n  1 handle: uint   # its handle\n  6 expiresAt:  time?  # when\n  7 n: int64?\n}\n\n\
                    read 0x0102 kv.get(kv.Call) -> kv.Entry\nwrite  0x010a kv.list(kv.List)   ->  download kv.Entry until kv.Page\n";
        let laid = "message kv.Call {\n  1 handle: uint      # its handle\n  6 expiresAt: time?  # when\n  7 n: int64?\n}\n\n\
                    read  0x0102 kv.get(kv.Call)  -> kv.Entry\nwrite 0x010a kv.list(kv.List) -> download kv.Entry until kv.Page\n";
        assert_eq!(format(text), laid);
        assert_eq!(format(laid), laid, "what is laid out comes back as it was");
    }

    #[test]
    fn comments_and_blank_lines_stay_as_they_were() {
        let text = "# kv, as its book has it\n\n# a call\nmessage kv.Page {\n  1 next: key?\n}\n";
        assert_eq!(format(text), text);
    }
}
