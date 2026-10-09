use std::time::SystemTime;

/// What a rate limit or a quota answers a request: whether it passed, and
/// what is left.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Allowance {
    pub ok: bool,
    /// How many more would pass now.
    pub left: u64,
    /// When the request would pass; `None` when it did.
    pub retry_at: Option<SystemTime>,
    /// A quota's windows, in the order it names them; none for a rate limit.
    pub windows: Vec<Window>,
}

/// One window of a quota, as a key stands in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Window {
    pub name: String,
    pub used: u64,
    pub limit: u64,
    pub left: u64,
    /// When the window ends, and the next use starts another; `None` for a
    /// window not started.
    pub resets_at: Option<SystemTime>,
}

impl Allowance {
    /// The window named `name`, of a quota's answer.
    pub fn window(&self, name: &str) -> Option<&Window> {
        self.windows.iter().find(|window| window.name == name)
    }
}
