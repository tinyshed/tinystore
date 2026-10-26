//! `tinystore config` as a shell runs it: a config as the store keeps it,
//! explained beside whoever holds the directory, and its live settings
//! changed and reset through the server that holds it, or a sidecar the
//! command starts.

use std::path::Path;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tinystore::config::{Env, Live, Required, Secret};
use tinystore::{ErrorKind, Options, Store};

const BINARY: &str = env!("CARGO_BIN_EXE_tinystore");

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Chat {
    region: Required<String>,
    token: Secret<String>,
    model: Model,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
struct Model {
    name: String,
    max_tokens: Live<u32>,
    window: Live<Duration>,
}

impl Default for Model {
    fn default() -> Self {
        Model { name: "small".to_owned(), max_tokens: Live::new(1024), window: Live::new(Duration::from_secs(30)) }
    }
}

/// The config as its program opens it, its required settings given by its
/// environment.
fn opened(store: &Store) -> tinystore::config::Config<Chat> {
    let env = Env::prefix("CHAT").from([("CHAT_REGION", "eu"), ("CHAT_TOKEN", "t0ken")]);
    store.config("chat", Chat::default()).env(env).open().unwrap()
}

fn tinystore(args: &[&dyn AsRef<std::ffi::OsStr>]) -> Output {
    Command::new(BINARY).args(args).output().unwrap()
}

fn said(output: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

/// The store once the server that held it has let it go.
fn stopped(dir: &Path) -> Store {
    let stop = tinystore(&[&"stop", &dir]);
    assert!(stop.status.success(), "{}", said(&stop));
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match Store::open(dir, Options::default()) {
            Ok(store) => return store,
            Err(error) if error.kind() == ErrorKind::InUse && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => panic!("{error}"),
        }
    }
}

// contract: CLI-05
#[test]
fn a_config_is_explained_as_the_store_keeps_it_and_its_live_settings_changed_from_a_shell() {
    let dirs = tempfile::tempdir().unwrap();
    let data = dirs.path().join("data");
    let store = Store::open(&data, Options::default()).unwrap();
    drop(opened(&store));
    store.close().unwrap();

    let explained = tinystore(&[&"config", &data, &"explain", &"chat"]);
    let text = said(&explained);
    assert!(explained.status.success(), "{text}");
    assert!(text.contains("model.max_tokens") && text.contains("1024") && text.contains("the declaration"), "{text}");
    assert!(text.contains("region: required, given by the program alone"), "{text}");
    assert!(!text.contains("t0ken"), "the program's environment is its own: {text}");

    let updated = tinystore(&[&"config", &data, &"update", &"chat", &"model.max_tokens=2048", &"model.window=1m"]);
    assert!(said(&updated).contains("updated config chat to version 1"), "{}", said(&updated));
    let fixed = tinystore(&[&"config", &data, &"update", &"chat", &"model.name=big"]);
    assert!(!fixed.status.success() && said(&fixed).contains("not live"), "{}", said(&fixed));
    let typo = tinystore(&[&"config", &data, &"update", &"chat", &"model.max_tokns=1"]);
    assert!(said(&typo).contains("did you mean model.max_tokens"), "{}", said(&typo));

    let json = tinystore(&[&"config", &data, &"explain", &"chat", &"--json"]);
    let shown: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    let line = shown["settings"].as_array().unwrap().iter().find(|line| line["path"] == "model.max_tokens").unwrap();
    assert_eq!((line["value"].as_str(), shown["version"].as_i64()), (Some("2048"), Some(1)), "{shown}");
    assert!(line["from"].as_str().unwrap().starts_with("a change, version 1"), "{shown}");

    let reset = tinystore(&[&"config", &data, &"reset", &"chat", &"model.window"]);
    assert!(said(&reset).contains("reset model.window of config chat: version 2"), "{}", said(&reset));
    let none = tinystore(&[&"config", &data, &"explain", &"chta"]);
    assert!(said(&none).contains("did you mean chat"), "{}", said(&none));

    let store = stopped(&data);
    let chat = opened(&store).get();
    assert_eq!((*chat.model.max_tokens, *chat.model.window), (2048, Duration::from_secs(30)));
}
