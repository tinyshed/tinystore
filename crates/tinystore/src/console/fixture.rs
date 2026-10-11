//! What the console's tests and a store logger's share: what a console
//! printed.

use std::io::{self, Write};
use std::sync::{Arc, Mutex, PoisonError};

/// What a console printed, as a writer a logger is told to print to.
#[derive(Clone, Default)]
pub(crate) struct Printed(Arc<Mutex<Vec<u8>>>);

impl Write for Printed {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Printed {
    pub(crate) fn lines(&self) -> Vec<String> {
        let printed = self.0.lock().unwrap_or_else(PoisonError::into_inner).clone();
        String::from_utf8(printed).unwrap().lines().map(str::to_owned).collect()
    }

    /// The JSON lines printed, each without its time, which is the clock's.
    pub(crate) fn untimed(&self) -> Vec<String> {
        let untimed = |line: &String| line.split_once("Z\",").map(|(_, rest)| format!("{{{rest}")).unwrap();
        self.lines().iter().map(untimed).collect()
    }
}
