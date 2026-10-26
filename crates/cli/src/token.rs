//! `tinystore token new`: a line for `serve`'s tokens file, a capability and
//! 32 random bytes in base64url, as the server reads them.
//!
//! ```text
//! tinystore token new admin >> tokens.txt
//! admin 4dGQ6tR2kq0SxVZpLWn8E1yHf7cJmB3aUoN9Tz5Ki0E
//! ```

use std::process::ExitCode;

use crate::asked::{self, Asked, Done, failed, wrong};
use crate::help;

pub(crate) fn run(args: &[String]) -> ExitCode {
    asked::run(&help::TOKEN, args, token)
}

fn token(asked: Asked) -> Done {
    let capability = match &asked.words[..] {
        [new] if new == "new" => "data",
        [new, capability] if new == "new" => match capability.as_str() {
            "admin" | "data" | "read" => capability.as_str(),
            other => return Err(wrong(format!("a token is admin, data or read, not {other}"))),
        },
        _ => return Err(wrong("token new [admin|data|read]")),
    };
    let mut random = [0; 32];
    getrandom::fill(&mut random).map_err(failed)?;
    outln!("{capability} {}", crate::local::base64url(&random))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_tokens_line_is_one_the_server_reads() {
        let mut random = [7u8; 32];
        random[0] = 0xfb;
        let line = format!("admin {}", crate::local::base64url(&random));
        assert_eq!(line.len(), "admin ".len() + 43);
        assert!(crate::remote::Tokens::parse(&line).is_ok(), "{line}");
    }
}
