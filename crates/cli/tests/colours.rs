//! The command line colours its words by the rule every logger keeps, so that
//! a command's help and the log lines printed beside it agree: here into a
//! pipe, where only `FORCE_COLOR` may colour, and `NO_COLOR` or a `TERM` of
//! `dumb` still win.

use std::process::Command;

const BINARY: &str = env!("CARGO_BIN_EXE_tinystore");

/// Whether `tinystore help` printed an escape sequence into a pipe, with
/// `set` the only colour variables of its environment.
fn coloured(set: &[(&str, &str)]) -> bool {
    let mut command = Command::new(BINARY);
    command.arg("help");
    for name in ["NO_COLOR", "FORCE_COLOR", "CLICOLOR_FORCE", "TERM"] {
        command.env_remove(name);
    }
    let output = command.envs(set.iter().copied()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    output.stdout.contains(&0x1b)
}

// contract: LOG-11
#[test]
fn the_help_into_a_pipe_is_coloured_as_a_logger_would_colour_it() {
    let cases: [(&[(&str, &str)], bool); 7] = [
        (&[], false),
        (&[("FORCE_COLOR", "1")], true),
        (&[("FORCE_COLOR", "0")], false),
        (&[("FORCE_COLOR", "1"), ("TERM", "dumb")], false),
        (&[("CLICOLOR_FORCE", "1")], false),
        (&[("FORCE_COLOR", "1"), ("NO_COLOR", "0")], false),
        (&[("FORCE_COLOR", "1"), ("NO_COLOR", "")], true),
    ];
    for (set, expected) in cases {
        assert_eq!(coloured(set), expected, "{set:?}");
    }
}
