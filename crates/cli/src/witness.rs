//! `tinystore witness`: a store that keeps each file's lease for the nodes
//! that name it as their failover, and no data, until Ctrl+C.
//!
//! ```text
//! tinystore witness ./witness --listen 0.0.0.0:7445 --token-file token
//!
//!   tinystore 0.1.0 · ./witness
//!
//!   ● witness  listen 0.0.0.0:7445        TLS · keeps the leases of the nodes that name it
//!
//!   Ctrl+C stops
//! ```

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use nu_ansi_term::{Color, Style};
use tinystore::replication::{Node, NodeTls};
use tinystore::{ErrorKind, Options, Store};

use crate::asked::{self, Asked, Done, failed, wrong};
use crate::help;
use crate::lines::paint;
use crate::serve::{Console, interrupted};

pub(crate) fn run(args: &[String]) -> ExitCode {
    asked::run(&help::WITNESS, args, witness)
}

fn witness(asked: Asked) -> Done {
    let (dir, rest) = asked.dir_needed("witness")?;
    if !rest.is_empty() {
        return Err(wrong(format!("one directory, not {} too", rest.join(" "))));
    }
    let listen = asked.value("--listen").ok_or_else(|| wrong("--listen <host>:<port>: where nodes reach it"))?;
    let token = match asked.value("--token-file") {
        Some(file) => Some(
            std::fs::read_to_string(file)
                .map_err(|error| failed(format!("--token-file {file}: {error}")))?
                .trim()
                .to_owned(),
        ),
        None => None,
    };
    let tls = match (asked.value("--tls-cert"), asked.value("--tls-key")) {
        (Some(cert), Some(key)) => {
            Some(NodeTls { cert: Some(PathBuf::from(cert)), key: Some(PathBuf::from(key)), roots: None })
        }
        (None, None) => None,
        _ => return Err(wrong("--tls-cert and --tls-key go together")),
    };
    let console = Console::open(None, asked.has("--quiet")).map_err(failed)?;
    let shown_tls = tls.is_some();
    let node = Node {
        serve: Some(listen.to_owned()),
        witness: true,
        token,
        tls,
        insecure: asked.has("--insecure"),
        ..Node::default()
    };
    let store =
        Store::open(&dir, Options { node: Some(node), ..Options::default() }).map_err(|error| match error.kind() {
            ErrorKind::InUse => failed(format!("{error}\nanother witness or node holds {}", dir.display())),
            _ => failed(error),
        })?;
    if console.person() {
        eprint!("{}", banner(console.colours(), &dir.display().to_string(), listen, shown_tls));
    }
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(failed)?;
    runtime.block_on(interrupted());
    tracing::info!(target: "tinystore", why = "interrupted", "stopping");
    let closing = Instant::now();
    store.close()?;
    tracing::info!(target: "tinystore", took = %format!("{:.1}s", closing.elapsed().as_secs_f64()), "stopped");
    Ok(())
}

fn banner(colours: bool, dir: &str, listen: &str, tls: bool) -> String {
    let paint = |style: Style, text: &str| paint(colours, style, text);
    let (bold, dim) = (Style::new().bold(), Style::new().dimmed());
    let link = if tls { "TLS".to_owned() } else { paint(Color::Yellow.normal(), "plain TCP") };
    format!(
        "\n  {} {} {} {}\n\n  {} {}  {}{} {} {}\n\n  {}\n\n",
        paint(bold, "tinystore"),
        paint(dim, env!("CARGO_PKG_VERSION")),
        paint(dim, "·"),
        paint(bold, dir),
        paint(Color::Green.bold(), "●"),
        paint(bold, "witness"),
        paint(Color::Cyan.normal(), &format!("listen {listen}")),
        " ".repeat(28usize.saturating_sub(listen.len() + 7)),
        link,
        paint(dim, "· keeps the leases of the nodes that name it as failover"),
        paint(dim, "Ctrl+C stops"),
    )
}
