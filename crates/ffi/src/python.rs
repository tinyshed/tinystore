//! The Python SDK's way to the core in its own process, `tinystore._core`:
//! the pipe as an extension module, carrying the wire's frames whole as the C
//! ABI does.
//!
//! No core thread ever waits for Python's lock. The SDK hands its frames in
//! with `send`, which answers nothing at once, and one thread of the SDK's
//! reads everything the core writes with `recv`, waiting with the lock let
//! go, then gives it to the event loop: the frames leave in one place, in
//! the order they were written.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use pyo3::exceptions::PyConnectionError;
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use tinystore::pipe::Pipe;

/// A connection in memory to the store in one directory.
#[pyclass(frozen, module = "tinystore._core", name = "Pipe")]
struct PythonPipe {
    /// None once closed. A read under way holds its own reference, so that
    /// closing never waits for it while holding the lock.
    pipe: Mutex<Option<Arc<Pipe>>>,
}

impl PythonPipe {
    fn pipe(&self) -> Option<Arc<Pipe>> {
        self.pipe.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

#[pymethods]
impl PythonPipe {
    /// Hands frames to the store. What they answer comes through `recv`.
    fn send(&self, py: Python<'_>, frames: &[u8]) {
        if let Some(pipe) = self.pipe() {
            py.detach(|| pipe.push(frames));
        }
    }

    /// The bytes the store wrote since the last read, waiting up to `wait_ms`
    /// for the first: none when nothing came, and None once the connection
    /// has ended and owes nothing more, as after the SDK's own GOAWAY.
    fn recv<'py>(&self, py: Python<'py>, wait_ms: u32) -> Option<Bound<'py, PyBytes>> {
        let pipe = self.pipe()?;
        let (bytes, ended) = py.detach(|| {
            let bytes = pipe.recv(Duration::from_millis(u64::from(wait_ms)));
            let ended = bytes.is_empty() && pipe.finished();
            (bytes, ended)
        });
        (!ended).then(|| PyBytes::new(py, &bytes))
    }

    /// Ends the connection; the store closes with its last one, before this
    /// returns unless a read under way still holds the pipe.
    fn close(&self, py: Python<'_>) {
        let taken = self.pipe.lock().unwrap_or_else(PoisonError::into_inner).take();
        py.detach(move || drop(taken));
    }
}

/// Opens the store in `dir`, or joins it when this process holds it already.
/// `options` is the wire's `store.Options`, no bytes for a store as it opens
/// unasked. A store that does not open raises ConnectionError with its code,
/// as the wire spells it, and its message.
#[pyfunction]
fn open(py: Python<'_>, dir: &str, options: &[u8]) -> PyResult<PythonPipe> {
    match py.detach(|| Pipe::open(dir, options, None)) {
        Ok(pipe) => Ok(PythonPipe { pipe: Mutex::new(Some(Arc::new(pipe))) }),
        Err(error) => {
            let code = error.kind().as_str().replace(' ', "_");
            Err(PyConnectionError::new_err((code, error.to_string())))
        }
    }
}

#[pymodule]
mod _core {
    #[pymodule_export]
    use super::{PythonPipe, open};
}
