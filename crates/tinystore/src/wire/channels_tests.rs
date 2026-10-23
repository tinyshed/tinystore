use std::time::Duration;

use crate::pipe::Capability;
use crate::pipe::fixture::{Client, answered};
use crate::wire::codec::Message;
use crate::wire::frame::{Frame, Kind};
use crate::wire::protocol::{ChannelsBatch, ChannelsPublish, ChannelsSubscribe, Empty, Hello, method};
use crate::{Options, Store};

fn publish(client: &mut Client, under: &[&str], value: &str) -> Result<Empty, crate::wire::protocol::Failure> {
    let under = under.iter().map(|owner| (*owner).to_owned()).collect();
    client.call(method::CHANNELS_PUBLISH, &ChannelsPublish { name: "chat".to_owned(), under, value: value.to_owned() })
}

/// Subscribes and reads the RESPONSE that says the store has the
/// subscription.
fn subscribed(client: &mut Client, under: &[&str], recursive: bool) -> u32 {
    let under = under.iter().map(|owner| (*owner).to_owned()).collect();
    let stream =
        client.start(method::CHANNELS_SUBSCRIBE, &ChannelsSubscribe { name: "chat".to_owned(), under, recursive });
    let begun = client.next_on(stream);
    assert_eq!((begun.kind, begun.flags), (Kind::Response, 0), "the subscription stays open");
    stream
}

/// The batches that arrive on `stream` until it is quiet a while.
fn batches(client: &mut Client, stream: u32) -> Vec<ChannelsBatch> {
    let mut batches = Vec::new();
    while !client.silent_on(stream, Duration::from_millis(100)) {
        batches.push(ChannelsBatch::decode(&client.next_on(stream).body).unwrap());
    }
    batches
}

// contract: CHAN-01
#[test]
fn a_subscription_answers_once_it_is_registered_then_sends_what_is_published_under_its_branch() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let stream = subscribed(&mut client, &["room-7"], false);
    publish(&mut client, &["room-7"], r#""hi""#).unwrap();
    publish(&mut client, &["room-9"], r#""elsewhere""#).unwrap();
    publish(&mut client, &["room-7", "typing"], r#"{"who":"ann"}"#).unwrap();
    let received: Vec<_> = batches(&mut client, stream).into_iter().flat_map(|batch| batch.messages).collect();
    assert_eq!(received.len(), 1, "the exact branch alone");
    assert_eq!(
        (received[0].under.as_slice(), received[0].value.as_str()),
        (["room-7".to_owned()].as_slice(), r#""hi""#)
    );

    let below = subscribed(&mut client, &["room-7"], true);
    publish(&mut client, &["room-7", "typing"], r#"{"who":"ann"}"#).unwrap();
    let typing: Vec<_> = batches(&mut client, below).into_iter().flat_map(|batch| batch.messages).collect();
    assert_eq!(typing[0].under, ["room-7", "typing"]);

    client.write(Frame::new(Kind::Cancel, stream, Vec::new()));
    assert_eq!(answered::<ChannelsBatch>(&client.next_on(stream)).unwrap_err().code, "cancelled");
}

// contract: CHAN-02
#[test]
fn a_client_that_grants_no_credit_falls_behind_and_is_told_how_many_it_missed() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::connect(dir.path());
    client.greet(Hello { protocol: 1, stream_credit: Some(4 << 10), ..Hello::default() }).unwrap();
    let stream = subscribed(&mut client, &[], false);
    let text = format!("{:?}", "m".repeat(100));
    let published = 2000;
    for _ in 0..published {
        publish(&mut client, &[], &text).unwrap();
    }
    let first = batches(&mut client, stream);
    let within: usize = first.iter().map(|batch| batch.messages.len()).sum();
    assert!(within > 0 && within < published, "what the credit took: {within}");
    let credit = (8u32 << 20).to_le_bytes().to_vec();
    client.write(Frame::new(Kind::Credit, stream, credit));
    let rest = batches(&mut client, stream);
    let after: usize = rest.iter().map(|batch| batch.messages.len()).sum();
    let missed: u64 = first.iter().chain(&rest).map(|batch| batch.missed).sum();
    assert!(rest[0].missed > 0, "the log let the oldest go, and says so before the next message");
    assert_eq!(within + after + missed as usize, published, "every message received or counted missed");
}

#[test]
fn a_message_larger_than_the_connection_takes_is_counted_missed_and_the_next_follows() {
    let dir = tempfile::tempdir().unwrap();
    let mut listener = Client::connect(dir.path());
    listener.greet(Hello { protocol: 1, stream_credit: Some(4 << 10), ..Hello::default() }).unwrap();
    let stream = subscribed(&mut listener, &[], false);
    let mut publisher = Client::open(dir.path());
    publish(&mut publisher, &[], &format!("{:?}", "p".repeat(5 << 10))).unwrap();
    publish(&mut publisher, &[], r#""small""#).unwrap();
    let received = batches(&mut listener, stream);
    let missed: u64 = received.iter().map(|batch| batch.missed).sum();
    let values: Vec<_> = received.into_iter().flat_map(|batch| batch.messages).map(|message| message.value).collect();
    assert_eq!((missed, values), (1, vec![r#""small""#.to_owned()]));
}

#[test]
fn a_read_connection_subscribes_but_publishes_nothing_and_a_publish_is_checked() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), Options::default()).unwrap();
    let mut reading = Client::admitted(&store, Capability::Read);
    subscribed(&mut reading, &[], false);
    assert_eq!(publish(&mut reading, &[], "1").unwrap_err().code, "permission");

    let mut writing = Client::admitted(&store, Capability::Data);
    assert_eq!(publish(&mut writing, &[], "{not json").unwrap_err().code, "invalid");
    let unnamed = ChannelsPublish { name: "Chat".to_owned(), under: Vec::new(), value: "1".to_owned() };
    assert_eq!(writing.call::<Empty>(method::CHANNELS_PUBLISH, &unnamed).unwrap_err().code, "invalid");
}

// contract: WIRE-03
#[test]
fn a_connection_whose_host_reads_nothing_stops_its_subscriptions_near_its_bound_and_goes_on_once_read() {
    let dir = tempfile::tempdir().unwrap();
    let mut listener = Client::connect(dir.path());
    listener.greet(Hello { protocol: 1, stream_credit: Some(64 << 20), ..Hello::default() }).unwrap();
    let stream = subscribed(&mut listener, &[], false);
    let mut publisher = Client::open(dir.path());
    let value = format!("{:?}", "p".repeat(60 << 10));
    let published = 400;
    for _ in 0..published {
        publish(&mut publisher, &[], &value).unwrap();
    }
    thread_settles();
    let owed = listener.pipe.recv(Duration::ZERO);
    assert!(owed.len() <= (8 << 20) + (4 << 20), "the connection owed {} bytes, near its bound", owed.len());

    let mut reader = crate::wire::frame::Reader::default();
    reader.push(&owed);
    let (mut received, mut missed) = (0, 0);
    loop {
        while let Some(frame) = reader.next(1 << 23).unwrap() {
            if frame.stream == stream && frame.kind == Kind::Data {
                let batch = ChannelsBatch::decode(&frame.body).unwrap();
                received += batch.messages.len();
                missed += batch.missed as usize;
            }
        }
        let bytes = listener.pipe.recv(Duration::from_millis(200));
        if bytes.is_empty() {
            break;
        }
        reader.push(&bytes);
    }
    assert!(missed > 0, "the log let go of what the connection could not take");
    assert_eq!(received + missed, published, "once read, the rest came, with what was missed");
}

/// A cancel that comes while a drain takes its batch from a busy channel's
/// log: the batch goes before the stream's final frame, or not at all.
// contract: WIRE-04
#[test]
fn a_subscription_cancelled_while_it_drains_sends_nothing_after_its_final_frame() {
    let dir = tempfile::tempdir().unwrap();
    let mut listener = Client::connect(dir.path());
    listener.greet(Hello { protocol: 1, stream_credit: Some(64 << 20), ..Hello::default() }).unwrap();
    let publishing = std::sync::atomic::AtomicBool::new(true);
    let mut ends = Ends::default();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let mut publisher = Client::open(dir.path());
            // batches of megabytes, so that a drain spends its time taking and encoding one
            let value = format!("{:?}", "m".repeat(16 << 10));
            while publishing.load(std::sync::atomic::Ordering::Relaxed) {
                publish(&mut publisher, &[], &value).unwrap();
            }
        });
        for stream in 1..=200 {
            let subscribe = ChannelsSubscribe { name: "chat".to_owned(), ..ChannelsSubscribe::default() };
            let request = Frame::new(Kind::Request, stream, subscribe.encode());
            ends.read(&listener.pipe.send(&bytes(Frame { method: method::CHANNELS_SUBSCRIBE, ..request }.ending())));
            ends.read(&listener.pipe.recv(Duration::from_millis(5)));
            ends.read(&listener.pipe.send(&bytes(Frame::new(Kind::Cancel, stream, Vec::new()))));
        }
        publishing.store(false, std::sync::atomic::Ordering::Relaxed);
    });
    ends.read(&listener.pipe.recv(Duration::from_millis(300)));
    assert_eq!(ends.ended.len(), 200, "every subscription ended");
    assert!(ends.late.is_empty(), "frames after their stream's final one: {:?}", ends.late);
}

fn bytes(frame: Frame) -> Vec<u8> {
    let mut bytes = Vec::new();
    frame.encode_into(&mut bytes);
    bytes
}

/// The streams whose final frame came, and the frames that came after one.
#[derive(Default)]
struct Ends {
    reader: crate::wire::frame::Reader,
    ended: Vec<u32>,
    late: Vec<(u32, Kind)>,
}

impl Ends {
    fn read(&mut self, bytes: &[u8]) {
        self.reader.push(bytes);
        while let Some(frame) = self.reader.next(1 << 23).unwrap() {
            if self.ended.contains(&frame.stream) {
                self.late.push((frame.stream, frame.kind));
            } else if frame.flags & crate::wire::frame::END != 0 {
                self.ended.push(frame.stream);
            }
        }
    }
}

/// Gives the session's workers a moment to send what they will.
fn thread_settles() {
    std::thread::sleep(Duration::from_millis(200));
}
