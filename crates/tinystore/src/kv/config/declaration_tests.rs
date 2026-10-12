use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;

use super::*;
use crate::kv::config::marks::{Live, Required, Secret, marked};

#[derive(Serialize, Deserialize, Default)]
struct Settings {
    port: u16,
    ratio: f64,
    timeout: Duration,
    region: Required<String>,
    token: Secret<String>,
    db: Db,
    upstreams: Vec<Upstream>,
    headers: BTreeMap<String, String>,
    proxy: Option<String>,
}

#[derive(Serialize, Deserialize, Default)]
struct Db {
    url: Secret<String>,
    pool: Live<u32>,
    zone: Live<Required<String>>,
}

#[derive(Serialize, Deserialize, Default)]
struct Upstream {
    host: Required<String>,
    port: u16,
}

fn declared<T: Serialize>(value: &T) -> Result<Declaration, String> {
    Declaration::of_marked(&marked(value).unwrap())
}

#[test]
fn a_rust_declaration_gives_each_setting_its_kind_default_and_marks() {
    let settings = Settings {
        port: 8080,
        timeout: Duration::from_millis(1500),
        token: Secret::new("t".into()),
        upstreams: vec![Upstream::default()],
        ..Settings::default()
    };
    let declaration = declared(&settings).unwrap();
    let kinds: Vec<(String, &Kind)> =
        declaration.settings().into_iter().map(|(path, setting)| (path, &setting.kind)).collect();
    let paths: Vec<&str> = kinds.iter().map(|(path, _)| path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "db.pool",
            "db.url",
            "db.zone",
            "headers",
            "port",
            "proxy",
            "ratio",
            "region",
            "timeout",
            "token",
            "upstreams"
        ]
    );
    let setting = |path: &str| declaration.setting(path).unwrap();
    assert_eq!((setting("port").kind.clone(), setting("port").default.clone()), (Kind::Integer, Some(json!(8080))));
    assert_eq!((setting("ratio").kind.clone(), setting("timeout").kind.clone()), (Kind::Number, Kind::Duration));
    assert_eq!(setting("timeout").default, Some(json!(1500)), "a span is kept as milliseconds");
    assert_eq!((setting("headers").kind.clone(), setting("proxy").kind.clone()), (Kind::Object, Kind::Any));
    assert!(setting("region").required && setting("region").default.is_none());
    assert!(setting("db.url").secret && setting("db.url").required, "a secret without a value is required");
    assert!(setting("token").secret && !setting("token").required && setting("token").default == Some(json!("t")));
    assert!(setting("db.pool").live && setting("db.zone").live && setting("db.zone").required);
    let Kind::List(item) = &setting("upstreams").kind else { panic!("a list") };
    let Item::Group(group) = &**item else { panic!("a list of groups") };
    let Some(Node::Setting(host)) = group.get("host") else { panic!("a host") };
    assert!(host.required && host.default.is_none());
}

#[test]
fn a_mark_that_means_nothing_is_refused() {
    #[derive(Serialize, Default)]
    struct LiveSecret {
        token: Live<Secret<String>>,
    }
    #[derive(Serialize, Default)]
    struct RequiredSecret {
        token: Required<Secret<String>>,
    }
    #[derive(Serialize, Default)]
    struct RequiredLive {
        pool: Required<Live<u32>>,
    }
    #[derive(Serialize, Default)]
    struct SecretNumber {
        pin: Secret<u32>,
    }
    #[derive(Serialize, Default)]
    struct LiveGroup {
        db: Live<Db>,
    }
    #[derive(Serialize, Default)]
    struct LiveInItem {
        items: Vec<Db>,
    }
    assert_eq!(declared(&LiveSecret::default()).unwrap_err(), "token: a secret is never live");
    assert_eq!(declared(&RequiredSecret::default()).unwrap_err(), "token: a secret is required already");
    assert_eq!(declared(&RequiredLive::default()).unwrap_err(), "pool: live is the outermost mark");
    assert_eq!(declared(&SecretNumber::default()).unwrap_err(), "pin: a secret is text");
    assert_eq!(declared(&LiveGroup::default()).unwrap_err(), "db: a mark goes on a setting, not on a group");
    let in_item = LiveInItem { items: vec![Db::default()] };
    assert_eq!(
        declared(&in_item).unwrap_err(),
        "items[0].pool: a list's item has no live field: the list is one setting"
    );
}

#[test]
fn a_declaration_finds_a_setting_or_a_group_by_its_path() {
    let declaration = declared(&Settings::default()).unwrap();
    assert!(matches!(declaration.node("db"), Some(Node::Group(_))));
    assert!(declaration.setting("db").is_none());
    assert!(declaration.setting("db.pool").is_some());
    assert!(declaration.node("db.pool.more").is_none());
    assert!(declaration.node("nothing").is_none());
}

#[test]
fn unmarked_takes_marks_off_and_writes_a_span_as_its_milliseconds() {
    let db = Db { url: Secret::new("u".into()), pool: Live::new(3), zone: Live::new(Required::new("eu".into())) };
    assert_eq!(unmarked(&marked(&db).unwrap()), json!({ "url": "u", "pool": 3, "zone": "eu" }));
    assert_eq!(unmarked(&json!({ "secs": 2, "nanos": 5_000_000 })), json!(2005));
}
