//! A node's settings file, `--node-config`: what a node is beyond where it
//! listens and whom it copies, one file in version control, each line with
//! its comment. A path in it is read from the file's own directory.
//!
//! ```toml
//! files = ["kv.db", "sql/app.db"]    # what it copies; every file but its own logs when absent
//! token-file = "/etc/tinystore/node-token"
//! catalog = false                     # true on the one node that keeps the catalog
//! advertise = "eu.internal:7444"      # where other nodes reach it; needed when it listens on 0.0.0.0
//! synchronous-with = ["eu2.internal:7444"]
//! failover = "witness.internal:7445"  # the witness that judges it
//! insecure = false                    # plain TCP beyond this machine, which anyone on the way reads
//!
//! [tls]
//! cert = "/etc/tinystore/node.pem"
//! key = "/etc/tinystore/node.key"
//! roots = "/etc/tinystore/ca.pem"
//! ```

use std::path::{Path, PathBuf};

use serde::Deserialize;
use tinystore::replication::NodeTls;

/// The settings a node's file holds, each as the file spells it.
#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) struct NodeConfig {
    pub(crate) files: Option<Vec<String>>,
    pub(crate) token_file: Option<PathBuf>,
    #[serde(default)]
    pub(crate) catalog: bool,
    pub(crate) advertise: Option<String>,
    #[serde(default)]
    pub(crate) synchronous: bool,
    #[serde(default)]
    pub(crate) synchronous_with: Vec<String>,
    pub(crate) failover: Option<String>,
    #[serde(default)]
    pub(crate) insecure: bool,
    pub(crate) tls: Option<Tls>,
}

#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Tls {
    pub(crate) cert: Option<PathBuf>,
    pub(crate) key: Option<PathBuf>,
    pub(crate) roots: Option<PathBuf>,
}

/// The settings a file may hold, for a misspelt one's nearest.
const SETTINGS: [&str; 10] = [
    "files",
    "token-file",
    "catalog",
    "advertise",
    "synchronous",
    "synchronous-with",
    "failover",
    "insecure",
    "tls",
    "cert",
];

impl NodeConfig {
    /// Reads a node's file, its paths made the file directory's.
    pub(crate) fn read(path: &Path) -> Result<NodeConfig, String> {
        let text = std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
        let mut config = NodeConfig::parse(&text).map_err(|why| format!("{}: {why}", path.display()))?;
        let base = path.parent().unwrap_or(Path::new("."));
        let within = |file: &mut Option<PathBuf>| {
            if let Some(file) = file.as_mut().filter(|file| file.is_relative()) {
                *file = base.join(&*file);
            }
        };
        within(&mut config.token_file);
        if let Some(tls) = &mut config.tls {
            within(&mut tls.cert);
            within(&mut tls.key);
            within(&mut tls.roots);
        }
        Ok(config)
    }

    fn parse(text: &str) -> Result<NodeConfig, String> {
        toml::from_str(text).map_err(|error| {
            let message = error.message().to_owned();
            let unknown = message.strip_prefix("unknown field `").and_then(|rest| rest.split('`').next());
            match unknown {
                Some(field) => format!("no setting {field}{}", crate::meant(field, &SETTINGS)),
                None => message,
            }
        })
    }

    /// Its token, read from its file.
    pub(crate) fn token(&self) -> Result<Option<String>, String> {
        let Some(file) = &self.token_file else { return Ok(None) };
        let token = std::fs::read_to_string(file).map_err(|error| format!("token-file {}: {error}", file.display()))?;
        Ok(Some(token.trim().to_owned()))
    }

    pub(crate) fn node_tls(&self) -> Option<NodeTls> {
        self.tls.as_ref().map(|tls| NodeTls { cert: tls.cert.clone(), key: tls.key.clone(), roots: tls.roots.clone() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_nodes_file_reads_as_written_and_a_misspelt_setting_is_named() {
        let config = NodeConfig::parse(
            "files = [\"kv.db\"]\ncatalog = true\nsynchronous-with = [\"eu2:7444\"]\n[tls]\nroots = \"ca.pem\"\n",
        )
        .unwrap();
        assert_eq!(config.files, Some(vec!["kv.db".to_owned()]));
        assert!(config.catalog && !config.insecure);
        assert_eq!(config.synchronous_with, ["eu2:7444"]);
        assert_eq!(config.tls.unwrap().roots, Some(PathBuf::from("ca.pem")));
        assert_eq!(
            NodeConfig::parse("advertize = \"x\"").unwrap_err(),
            "no setting advertize; did you mean advertise?"
        );
        assert!(NodeConfig::parse("catalog = \"yes\"").is_err());
    }

    #[test]
    fn a_path_in_a_nodes_file_is_read_from_the_files_directory() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("node.toml");
        std::fs::write(&file, "token-file = \"token\"\n[tls]\ncert = \"node.pem\"\n").unwrap();
        std::fs::write(dir.path().join("token"), "secret\n").unwrap();
        let config = NodeConfig::read(&file).unwrap();
        assert_eq!(config.token().unwrap().as_deref(), Some("secret"));
        assert_eq!(config.node_tls().unwrap().cert, Some(dir.path().join("node.pem")));
    }
}
