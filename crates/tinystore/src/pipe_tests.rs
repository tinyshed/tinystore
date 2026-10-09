use super::*;
use crate::wire::frame::{self, Frame, Kind};
use crate::wire::msgpack::{self, Value};

/// A client's side of a pipe: frames out, frames back, as an SDK speaks.
struct Client {
    pipe: Pipe,
    reader: frame::Reader,
    next_stream: u32,
}

impl Client {
    fn open(dir: &Path) -> Client {
        let pipe = Pipe::open(dir, None).unwrap();
        let mut client = Client { pipe, reader: frame::Reader::default(), next_stream: 0 };
        let hello = map(vec![(1, Value::Uint(1)), (2, Value::Str("test/0".to_owned()))]);
        client.write(Frame::new(Kind::Hello, 0, hello));
        let welcome = client.read();
        assert_eq!(welcome.kind, Kind::Welcome);
        client
    }

    fn write(&mut self, frame: Frame) -> Vec<u8> {
        let mut bytes = Vec::new();
        frame.encode_into(&mut bytes);
        let ready = self.pipe.send(&bytes);
        self.reader.push(&ready);
        bytes
    }

    fn read(&mut self) -> Frame {
        loop {
            if let Some(frame) = self.reader.next(1 << 22).unwrap() {
                return frame;
            }
            let bytes = self.pipe.recv(Duration::from_secs(10));
            assert!(!bytes.is_empty(), "the pipe answered nothing within ten seconds");
            self.reader.push(&bytes);
        }
    }

    /// Calls a method and returns its answer's fields, or its error's.
    fn call(&mut self, method: u16, fields: Vec<(u64, Value)>) -> (bool, Vec<(Value, Value)>) {
        self.next_stream += 1;
        let request = Frame { method, ..Frame::new(Kind::Request, self.next_stream, map(fields)) }.ending();
        self.write(request);
        let answer = self.read();
        assert_eq!((answer.kind, answer.stream), (Kind::Response, self.next_stream));
        let Value::Map(pairs) = msgpack::decode(&answer.body).unwrap() else { panic!("an answer that is not a map") };
        (answer.flags & frame::ERROR == 0, pairs)
    }
}

fn map(fields: Vec<(u64, Value)>) -> Vec<u8> {
    msgpack::encode(&Value::Map(fields.into_iter().map(|(key, value)| (Value::Uint(key), value)).collect()))
}

fn field(pairs: &[(Value, Value)], number: u64) -> Option<&Value> {
    pairs.iter().find(|(key, _)| *key == Value::Uint(number)).map(|(_, value)| value)
}

#[test]
fn a_client_sets_and_gets_a_key_through_the_pipe() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let (ok, opened) = client.call(0x0101, vec![(1, Value::Str("sessions".to_owned()))]);
    assert!(ok);
    let handle = field(&opened, 1).unwrap().clone();

    let owners = Value::Array(vec![Value::Str("7".to_owned())]);
    let key = Value::Str("token".to_owned());
    let set = vec![(1, handle.clone()), (2, owners.clone()), (3, key.clone()), (4, Value::Bin(b"hello".to_vec()))];
    let (ok, stamp) = client.call(0x0104, set);
    assert!(ok, "{stamp:?}");
    assert_eq!(field(&stamp, 1), Some(&Value::Bool(true)));

    let (ok, entry) = client.call(0x0102, vec![(1, handle), (2, owners), (3, key)]);
    assert!(ok);
    assert_eq!(field(&entry, 1), Some(&Value::Bool(true)));
    assert_eq!(field(&entry, 2), Some(&Value::Bin(b"hello".to_vec())));
    assert_eq!(field(&entry, 3), field(&stamp, 3), "the version the set gave");
}

#[test]
fn a_method_the_core_does_not_have_is_unimplemented() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let (ok, failure) = client.call(0x0601, vec![]);
    assert!(!ok);
    assert_eq!(field(&failure, 1), Some(&Value::Str("unimplemented".to_owned())));
}

#[test]
fn a_frame_before_hello_ends_the_pipe_with_goaway() {
    let dir = tempfile::tempdir().unwrap();
    let pipe = Pipe::open(dir.path(), None).unwrap();
    let mut bytes = Vec::new();
    Frame::new(Kind::Ping, 0, vec![0; 8]).encode_into(&mut bytes);
    let mut reader = frame::Reader::default();
    reader.push(&pipe.send(&bytes));
    assert_eq!(reader.next(1 << 20).unwrap().unwrap().kind, Kind::GoAway);
}

#[test]
fn pipes_on_one_directory_share_its_store_until_the_last_closes() {
    let dir = tempfile::tempdir().unwrap();
    let first = Client::open(dir.path());
    let second = Client::open(dir.path());
    assert!(Store::open(dir.path(), Options::default()).is_err(), "the pipes' store holds the LOCK");
    drop(first);
    assert!(Store::open(dir.path(), Options::default()).is_err(), "one pipe still holds it");
    drop(second);
    Store::open(dir.path(), Options::default()).unwrap().close().unwrap();
}
