use serde_json::{Value, json};

use crate::pipe::Capability;
use crate::pipe::fixture::{Client, answered};
use crate::wire::codec::Message;
use crate::wire::frame::{Frame, Kind};
use crate::wire::protocol::{
    ConfigGiven, ConfigOpen, ConfigOrigin, ConfigReset, ConfigUpdate, ConfigValue, Empty, Handle, method,
};
use crate::{Options, Store};

fn declaration() -> String {
    let rps = json!({ "kind": "integer", "default": 100, "min": 0, "live": true });
    json!({ "port": { "kind": "integer", "default": 8080 }, "limits": { "group": { "rps": rps } } }).to_string()
}

fn open(client: &mut Client, layers: &Value) -> ConfigValue {
    let open = ConfigOpen {
        name: "app".to_owned(),
        declaration: declaration(),
        layers: layers.to_string(),
        strict: false,
        kept: false,
    };
    client.call(method::CONFIG_OPEN, &open).unwrap()
}

fn value(message: &ConfigValue) -> Value {
    serde_json::from_str(&message.value).unwrap()
}

fn rps(message: &ConfigValue) -> Value {
    value(message)["limits"]["rps"].clone()
}

#[test]
fn a_config_opened_over_the_wire_reads_the_layers_its_program_sent() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let opened = open(&mut client, &json!([{ "env": { "prefix": "APP", "variables": { "APP_PORT": "9000" } } }]));
    assert_eq!(value(&opened)["port"], json!(9000));
    let port = opened.lines.iter().find(|line| line.path == "port").unwrap();
    assert_eq!((port.value.as_str(), port.from.as_str()), ("9000", "APP_PORT"));
    let broken = ConfigOpen {
        name: "app".to_owned(),
        declaration: "{".to_owned(),
        layers: "[]".to_owned(),
        strict: false,
        kept: false,
    };
    assert_eq!(client.call::<ConfigValue>(method::CONFIG_OPEN, &broken).unwrap_err().code, "invalid");
    let unknown = ConfigReset { handle: 99, path: String::new() };
    assert_eq!(client.call::<ConfigValue>(method::CONFIG_RESET, &unknown).unwrap_err().code, "invalid");
}

#[test]
fn a_config_opened_as_kv_db_keeps_it_takes_the_declaration_kept_there_and_changes_its_live_settings() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let kept = ConfigOpen { name: "app".to_owned(), kept: true, ..ConfigOpen::default() };
    let none = client.call::<ConfigValue>(method::CONFIG_OPEN, &kept).unwrap_err();
    assert!(none.code == "not_found" && none.message.contains("keeps no declaration"), "{none:?}");
    let required = json!({ "kind": "text", "required": true });
    let live = json!({ "kind": "integer", "default": 100, "live": true });
    let declared = json!({ "region": required, "limits": { "group": { "rps": live } } }).to_string();
    let layers = json!([{ "env": { "prefix": "APP", "variables": { "APP_REGION": "eu" } } }]).to_string();
    let program = ConfigOpen { name: "app".to_owned(), declaration: declared, layers, strict: false, kept: false };
    client.call::<ConfigValue>(method::CONFIG_OPEN, &program).unwrap();
    let opened: ConfigValue = client.call(method::CONFIG_OPEN, &kept).unwrap();
    assert_eq!(value(&opened).get("region"), None, "the program's environment is its own");
    assert!(opened.notes.iter().any(|note| note.starts_with("region: required")), "{:?}", opened.notes);
    let changes = json!({ "limits": { "rps": 250 } }).to_string();
    let update = ConfigUpdate { handle: opened.handle, version: opened.version, changes };
    let changed: ConfigValue = client.call(method::CONFIG_UPDATE, &update).unwrap();
    assert_eq!((rps(&changed), changed.version), (json!(250), opened.version + 1));
}

// contract: CFG-09
#[test]
fn a_line_on_the_wire_gives_every_value_with_only_the_fields_its_kind_names() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let layers = json!([
        { "object": { "port": 8100 }, "place": 2 },
        { "file": { "name": "app.yaml", "text": "port: 8200\n" } },
        { "env": {
            "prefix": "APP",
            "variables": { "APP_PORT_FILE": "/run/secrets/port" },
            "files": { "/run/secrets/port": "9000\n" },
            "dotenv": [{ "name": ".env", "text": "APP_LIMITS_RPS=150\n" }]
        } }
    ]);
    let opened = open(&mut client, &layers);
    let changes = json!({ "limits": { "rps": 200 } }).to_string();
    let update = ConfigUpdate { handle: opened.handle, version: opened.version, changes };
    let changed: ConfigValue = client.call(method::CONFIG_UPDATE, &update).unwrap();
    let given = |path: &str| changed.lines.iter().find(|line| line.path == path).unwrap().given.clone();
    let origin = |kind: &str| ConfigOrigin { kind: kind.to_owned(), ..ConfigOrigin::default() };
    let gave = |value: &str, from: ConfigOrigin| ConfigGiven { value: value.to_owned(), from };
    let named = |variable: &str| Some(variable.to_owned());
    assert_eq!(
        given("port"),
        [
            gave(
                "9000",
                ConfigOrigin { variable: named("APP_PORT_FILE"), file: named("/run/secrets/port"), ..origin("env") }
            ),
            gave("8200", ConfigOrigin { path: named("app.yaml"), ..origin("file") }),
            gave("8100", ConfigOrigin { place: Some(2), ..origin("layer") }),
            gave("8080", origin("declaration")),
        ]
    );
    let rps = given("limits.rps");
    let at = rps[0].from.at.expect("a change says when it was made");
    let now = crate::unix_millis(std::time::SystemTime::now());
    assert!((now - 60_000..=now).contains(&at), "a change made at {at}, read at {now}");
    assert_eq!(
        rps,
        [
            gave("200", ConfigOrigin { version: Some(changed.version), at: Some(at), ..origin("change") }),
            gave("150", ConfigOrigin { variable: named("APP_LIMITS_RPS"), dotenv: named(".env"), ..origin("env") }),
            gave("100", origin("declaration")),
        ]
    );
}

// contract: CFG-04
#[test]
fn a_follow_sends_the_value_now_then_each_change_until_cancelled() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let opened = open(&mut client, &json!([]));
    let stream = client.start(method::CONFIG_FOLLOW, &Handle { handle: opened.handle });
    let begun = client.next_on(stream);
    assert_eq!((begun.kind, begun.flags), (Kind::Response, 0), "the follow stays open");
    let now = ConfigValue::decode(&client.next_on(stream).body).unwrap();
    assert_eq!((now.version, rps(&now)), (opened.version, json!(100)));
    let changes = json!({ "limits": { "rps": 200 } }).to_string();
    let update = ConfigUpdate { handle: opened.handle, version: opened.version, changes };
    let changed: ConfigValue = client.call(method::CONFIG_UPDATE, &update).unwrap();
    assert_eq!(rps(&changed), json!(200), "the answer holds the value the change made");
    let followed = ConfigValue::decode(&client.next_on(stream).body).unwrap();
    assert_eq!((followed.version, rps(&followed)), (changed.version, json!(200)));
    assert_eq!(client.call::<ConfigValue>(method::CONFIG_UPDATE, &update).unwrap_err().code, "conflict");
    let reset = ConfigReset { handle: opened.handle, path: "limits.rps".to_owned() };
    let back: ConfigValue = client.call(method::CONFIG_RESET, &reset).unwrap();
    assert_eq!(rps(&back), json!(100));
    assert_eq!(rps(&ConfigValue::decode(&client.next_on(stream).body).unwrap()), json!(100));
    client.write(Frame::new(Kind::Cancel, stream, Vec::new()));
    assert_eq!(answered::<Empty>(&client.next_on(stream)).unwrap_err().code, "cancelled");
}

#[test]
fn a_read_connection_opens_and_follows_a_config_and_changes_none() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), Options::default()).unwrap();
    let mut client = Client::admitted(&store, Capability::Read);
    let opened = open(&mut client, &json!([]));
    let stream = client.start(method::CONFIG_FOLLOW, &Handle { handle: opened.handle });
    assert_eq!(client.next_on(stream).kind, Kind::Response);
    let update = ConfigUpdate { handle: opened.handle, version: opened.version, changes: "{}".to_owned() };
    assert_eq!(client.call::<ConfigValue>(method::CONFIG_UPDATE, &update).unwrap_err().code, "permission");
    let reset = ConfigReset { handle: opened.handle, path: String::new() };
    assert_eq!(client.call::<ConfigValue>(method::CONFIG_RESET, &reset).unwrap_err().code, "permission");
}
