use serde_json::{Value, json};

use crate::pipe::Capability;
use crate::pipe::fixture::{Client, answered};
use crate::wire::codec::Message;
use crate::wire::frame::{Frame, Kind};
use crate::wire::protocol::{ConfigOpen, ConfigReset, ConfigUpdate, ConfigValue, Empty, Handle, method};
use crate::{Options, Store};

fn declaration() -> String {
    let rps = json!({ "kind": "integer", "default": 100, "min": 0, "live": true });
    json!({ "port": { "kind": "integer", "default": 8080 }, "limits": { "group": { "rps": rps } } }).to_string()
}

fn open(client: &mut Client, layers: &Value) -> ConfigValue {
    let open =
        ConfigOpen { name: "app".to_owned(), declaration: declaration(), layers: layers.to_string(), strict: false };
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
    let broken =
        ConfigOpen { name: "app".to_owned(), declaration: "{".to_owned(), layers: "[]".to_owned(), strict: false };
    assert_eq!(client.call::<ConfigValue>(method::CONFIG_OPEN, &broken).unwrap_err().code, "invalid");
    let unknown = ConfigReset { handle: 99, path: String::new() };
    assert_eq!(client.call::<ConfigValue>(method::CONFIG_RESET, &unknown).unwrap_err().code, "invalid");
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
