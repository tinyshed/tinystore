use std::time::Duration;

/// How far a write goes before it returns, and so what a crash can take.
///
/// | Value        | A write returns once it is     | It survives                             |
/// |--------------|--------------------------------|-----------------------------------------|
/// | `Full`       | synced to the disk             | a crash of the application or the OS    |
/// |              |                                | and a power loss                        |
/// | `Os`         | handed to the operating system | a crash of the application              |
/// | `Every(1 s)` | in memory, written each second | everything but the last second          |
///
/// A `Duration` is the interval: `.durability(Duration::from_secs(1))`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Durability {
    Full,
    Os,
    Every(Duration),
}

impl From<Duration> for Durability {
    fn from(interval: Duration) -> Self {
        Durability::Every(interval)
    }
}
