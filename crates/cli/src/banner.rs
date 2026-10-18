//! What `serve` shows a person who started it in a terminal: what it serves,
//! how each kind of client reaches it, what is off and how to turn it on, the
//! store's files, and what to type next.
//!
//! ```text
//!   tinystore 0.1.0 · ./data
//!
//!   ● local    programs on this machine     pipe tinystore-5434ed53 · found through data/server/SERVE
//!   ○ remote   off                          --listen tls://<host>:<port> serves other machines
//!   ○ node     off                          --node-listen <host>:<port> lets other nodes copy it
//!
//!   kv 36 KiB · jobs 60 KiB · sql/app 1.2 MiB
//!   ready in 14 ms · Ctrl+C stops · tinystore logs ./data -f · tinystore status ./data
//! ```

use std::path::Path;
use std::time::Duration;

use nu_ansi_term::{Color, Style};

use crate::args;
use crate::files;
use crate::lines::paint;

/// What the banner says beyond where the server listens: what the person
/// typed and asked for, taken before the store opened.
#[derive(Debug, Default)]
pub(crate) struct Shown {
    /// The directory as the person typed it.
    pub(crate) dir: String,
    /// The directory held no store before this server opened it.
    pub(crate) new_store: bool,
    /// The capabilities of a remote server's tokens, in their file's order.
    pub(crate) tokens: Vec<String>,
    /// What the node flags said.
    pub(crate) node: Option<NodeShown>,
}

#[derive(Debug, Default)]
pub(crate) struct NodeShown {
    pub(crate) listen: Option<String>,
    pub(crate) from: Vec<String>,
    pub(crate) files: Option<Vec<String>>,
    pub(crate) catalog: bool,
    pub(crate) advertise: Option<String>,
    pub(crate) failover: Option<String>,
    pub(crate) synchronous_with: Vec<String>,
    pub(crate) synchronous: bool,
    pub(crate) tls: bool,
    /// The file `--node-config` named, by its name alone.
    pub(crate) config: Option<String>,
}

impl Shown {
    /// What the banner shows of `serve`, read before the store opens.
    pub(crate) fn of(serve: &args::Serve) -> Shown {
        let new_store = std::fs::read_dir(&serve.dir).map_or(true, |mut entries| entries.next().is_none());
        let tokens = serve.remote.as_ref().map(|remote| capabilities(&remote.tokens)).unwrap_or_default();
        let node = serve.node.as_ref().map(|node| {
            let settings = node.settings();
            NodeShown {
                listen: node.listen.clone(),
                from: node.from.clone(),
                files: settings.files.clone(),
                catalog: settings.catalog,
                advertise: settings.advertise.clone(),
                failover: settings.failover.clone(),
                synchronous_with: settings.synchronous_with.clone(),
                synchronous: settings.synchronous,
                tls: settings.tls.is_some(),
                config: node
                    .config
                    .as_ref()
                    .and_then(|(file, _)| file.file_name())
                    .map(|name| name.to_string_lossy().into_owned()),
            }
        });
        Shown { dir: serve.dir.display().to_string(), new_store, tokens, node }
    }
}

/// The first word of each line of a tokens file that is not blank or a
/// comment: the capability it gives.
fn capabilities(tokens: &Path) -> Vec<String> {
    let read = std::fs::read_to_string(tokens).unwrap_or_default();
    let lines = read.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#'));
    lines.filter_map(|line| line.split_whitespace().next()).map(str::to_owned).collect()
}

/// The banner, coloured when `colours`: `store` is the directory the server
/// opened, `endpoints` where it listens, the local one first.
pub(crate) fn text(colours: bool, shown: &Shown, store: &Path, endpoints: &[String], ready: Duration) -> String {
    let paint = |style: Style, text: &str| paint(colours, style, text);
    let (bold, dim, on, address) =
        (Style::new().bold(), Style::new().dimmed(), Color::Green.bold(), Color::Cyan.normal());
    let mut banner = heading(colours, shown);
    // a row with no label goes on the one above it
    let mut row = |lit: bool, label: &str, what: String, how: String| {
        let (dot, label) = match (label.is_empty(), lit) {
            (true, _) => (" ".to_owned(), " ".repeat(8)),
            (false, true) => (paint(on, "●"), paint(bold, &format!("{label:<8}"))),
            (false, false) => (paint(dim, "○"), paint(dim, &format!("{label:<8}"))),
        };
        banner.push_str(&format!("  {dot} {label} {what} {how}\n"));
    };
    let column = |text: &str, style: Style| {
        format!("{}{}", paint(style, text), " ".repeat(28usize.saturating_sub(text.chars().count())))
    };
    let local = endpoints.first().map_or_else(String::new, |endpoint| local_name(endpoint));
    let found = format!("found through {}/server/SERVE", shown.dir.trim_end_matches(['/', '\\']));
    row(
        true,
        "local",
        column("programs on this machine", Style::new()),
        format!("{} {} {}", paint(address, &local), paint(dim, "·"), paint(dim, &found)),
    );
    match endpoints.get(1) {
        Some(remote) => {
            let tokens = match shown.tokens.len() {
                0 => paint(Color::Yellow.normal(), "no tokens"),
                count => format!(
                    "{} {}",
                    count,
                    paint(dim, &format!("token{}: {}", if count == 1 { "" } else { "s" }, shown.tokens.join(", ")))
                ),
            };
            row(true, "remote", column(remote, address), tokens);
        }
        None => {
            row(false, "remote", column("off", dim), paint(dim, "--listen tls://<host>:<port> serves other machines"))
        }
    }
    match &shown.node {
        Some(node) => node_rows(&mut row, &column, node, colours),
        None => {
            row(false, "node", column("off", dim), paint(dim, "--node-listen <host>:<port> lets other nodes copy it"))
        }
    }
    banner.push_str(&footing(colours, shown, store, ready));
    banner
}

/// The banner's first line: the version and the store's directory.
fn heading(colours: bool, shown: &Shown) -> String {
    let paint = |style: Style, text: &str| paint(colours, style, text);
    let (bold, dim) = (Style::new().bold(), Style::new().dimmed());
    let new = if shown.new_store { format!("  {}", paint(Color::Yellow.normal(), "new store")) } else { String::new() };
    format!(
        "\n  {} {} {} {}{new}\n\n",
        paint(bold, "tinystore"),
        paint(dim, env!("CARGO_PKG_VERSION")),
        paint(dim, "·"),
        paint(bold, &shown.dir)
    )
}

/// The banner's last lines: the store's files, and the commands that come next.
fn footing(colours: bool, shown: &Shown, store: &Path, ready: Duration) -> String {
    let paint = |style: Style, text: &str| paint(colours, style, text);
    let dim = Style::new().dimmed();
    format!(
        "\n  {}\n  {} {} {} {}\n\n",
        files_line(colours, store),
        paint(dim, &format!("ready in {} ms · Ctrl+C stops ·", ready.as_millis())),
        paint(Style::new(), &format!("tinystore logs {} -f", shown.dir)),
        paint(dim, "·"),
        paint(Style::new(), &format!("tinystore status {}", shown.dir)),
    )
}

/// The node's rows: where it listens and what it copies, then what decides
/// its writes.
fn node_rows(
    row: &mut impl FnMut(bool, &str, String, String),
    column: &impl Fn(&str, Style) -> String,
    node: &NodeShown,
    colours: bool,
) {
    let paint = |style: Style, text: &str| paint(colours, style, text);
    let (dim, address) = (Style::new().dimmed(), Color::Cyan.normal());
    let listen = node.listen.as_ref().map_or_else(|| "serves no node".to_owned(), |listen| format!("listen {listen}"));
    let mut said = Vec::new();
    if let Some(advertise) = &node.advertise {
        said.push(format!("advertised as {}", paint(address, advertise)));
    }
    said.push(if node.tls { "TLS".to_owned() } else { paint(Color::Yellow.normal(), "plain TCP") });
    if node.catalog {
        said.push("keeps the catalog".to_owned());
    }
    row(true, "node", column(&listen, address), said.join(&paint(dim, " · ")));
    if !node.from.is_empty() {
        let files = node.files.as_ref().map_or_else(|| "every file".to_owned(), |files| files.join(", "));
        let from = format!("copies {}", node.from.join(", "));
        let config = node.config.as_ref().map_or_else(String::new, |config| format!(" ({config})"));
        row_more(row, column(&from, Style::new()), paint(dim, &format!("{files}{config}")));
    } else if let Some(config) = &node.config {
        row_more(row, column("", Style::new()), paint(dim, &format!("settings from {config}")));
    }
    let mut decides = Vec::new();
    if let Some(failover) = &node.failover {
        decides.push(format!("failover {}", paint(address, failover)));
    }
    if !node.synchronous_with.is_empty() {
        decides.push(format!("synchronous with {}", node.synchronous_with.join(", ")));
    } else if node.synchronous {
        decides.push("synchronous with any copy".to_owned());
    }
    if !decides.is_empty() {
        row_more(row, column("", Style::new()), decides.join(&paint(dim, " · ")));
    }
}

/// A row that goes on the one above, with no dot or label.
fn row_more(row: &mut impl FnMut(bool, &str, String, String), what: String, how: String) {
    row(true, "", what, how);
}

/// The store's files and their sizes, each with its WAL: `kv 36 KiB · jobs
/// 60 KiB · sql/app 1.2 MiB`, or `empty`.
fn files_line(colours: bool, store: &Path) -> String {
    let dim = Style::new().dimmed();
    let found = files::of(store);
    if found.is_empty() {
        return paint(colours, dim, "empty: each engine makes its file the first time a program opens it");
    }
    let shown: Vec<String> =
        found.iter().map(|file| format!("{} {}", file.name, paint(colours, dim, files::bytes(file.bytes)))).collect();
    shown.join(&paint(colours, dim, " · "))
}

/// A local endpoint as a person reads it: `pipe tinystore-5434ed53` or
/// `socket /srv/data/server/tinystore.sock`.
fn local_name(endpoint: &str) -> String {
    if let Some(pipe) = endpoint.strip_prefix("pipe:") {
        return format!("pipe {pipe}");
    }
    if let Some(socket) = endpoint.strip_prefix("unix://") {
        return format!("socket {socket}");
    }
    endpoint.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_banner_says_what_is_on_what_is_off_and_how_to_turn_it_on() {
        let dir = tempfile::tempdir().unwrap();
        let shown = Shown { dir: "./data".to_owned(), new_store: true, ..Shown::default() };
        let banner =
            text(false, &shown, dir.path(), &["pipe:tinystore-5434ed53".to_owned()], Duration::from_millis(14));
        assert!(banner.contains("tinystore") && banner.contains("./data  new store"), "{banner}");
        assert!(banner.contains("● local") && banner.contains("pipe tinystore-5434ed53"), "{banner}");
        assert!(banner.contains("○ remote") && banner.contains("--listen tls://"), "{banner}");
        assert!(banner.contains("○ node") && banner.contains("--node-listen"), "{banner}");
        assert!(banner.contains("empty") && banner.contains("tinystore logs ./data -f"), "{banner}");
        assert!(!banner.contains('\u{1b}'));
    }
}
