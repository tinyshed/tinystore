//! What a child writes to its stdout, which whoever started it reads through
//! a pipe: a private child's frames and an MCP server's answers. Shared with
//! the command line, it promises a program nothing.

use std::io::{self, Write};
use std::thread;
use std::time::Duration;

/// How long a write that took nothing waits before it tries again, at first.
const FIRST_PAUSE: Duration = Duration::from_millis(1);
/// The longest pause, which an output that keeps taking nothing is tried at.
const LONGEST_PAUSE: Duration = Duration::from_millis(50);

/// Writes every byte, as `write_all` does, except that a write that took
/// none is tried again after a pause, longer each time it takes none, for as
/// long as it takes none: a blocking write to a pipe nobody reads would wait
/// as long.
///
/// Under Node on Windows, a blocking write to the pipe of a child's stdout
/// now and then takes no byte and answers that it succeeded: a write a little
/// over the pipe's 64 KiB, begun as Node's reader empties it. Tried again, it
/// takes every byte at once. `write_all` takes that answer for the end of the
/// pipe, and an answer stopped part way while its reader waited for the rest.
/// A pipe that is gone fails with an error, never with a write of nothing.
pub fn write_whole(to: &mut impl Write, mut bytes: &[u8]) -> io::Result<()> {
    let mut pause = FIRST_PAUSE;
    while !bytes.is_empty() {
        match to.write(bytes) {
            Ok(0) => {
                thread::sleep(pause);
                pause = (pause * 2).min(LONGEST_PAUSE);
            }
            Ok(taken) => {
                bytes = &bytes[taken..];
                pause = FIRST_PAUSE;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::{self, Write};

    use super::write_whole;

    /// An output that takes nothing on three writes of every four, and at
    /// most 1000 bytes on the fourth.
    #[derive(Default)]
    struct Hesitant {
        taken: Vec<u8>,
        writes: usize,
        refused: usize,
    }

    impl Write for Hesitant {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.writes += 1;
            if !self.writes.is_multiple_of(4) {
                self.refused += 1;
                return Ok(0);
            }
            let taken = bytes.len().min(1000);
            self.taken.extend_from_slice(&bytes[..taken]);
            Ok(taken)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_write_that_takes_nothing_is_tried_again_until_every_byte_is_taken() {
        let bytes: Vec<u8> = (0..5500u32).map(|at| (at % 251) as u8).collect();
        let mut to = Hesitant::default();
        write_whole(&mut to, &bytes).unwrap();
        assert_eq!(to.taken, bytes, "every byte, in order");
        // six writes of 1000 bytes or fewer took them, each after three that took none
        assert_eq!(to.refused, 18);
    }

    /// An output whose writes fail, as a pipe's whose reader is gone.
    struct Gone {
        writes: usize,
    }

    impl Write for Gone {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            self.writes += 1;
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "the reader is gone"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_write_that_fails_ends_with_its_error_and_is_not_tried_again() {
        let mut to = Gone { writes: 0 };
        let failed = write_whole(&mut to, b"an answer").unwrap_err();
        assert_eq!((failed.kind(), failed.to_string()), (io::ErrorKind::BrokenPipe, "the reader is gone".into()));
        assert_eq!(to.writes, 1);
    }
}
