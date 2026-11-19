use crate::pipe::fixture::Client;
use crate::wire::protocol::{Handle, KvBranch, KvBucketOpen, KvCall, KvCount, KvWritten, method};

// contract: KV-15
#[test]
fn a_count_over_the_wire_is_the_branchs_own_keys() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let open = KvBucketOpen { name: "sessions".to_owned(), ..KvBucketOpen::default() };
    let handle = client.call::<Handle>(method::KV_BUCKET_OPEN, &open).unwrap().handle;
    for (under, key) in [(vec![], "a"), (vec!["42".to_owned()], "phone"), (vec!["42".to_owned()], "laptop")] {
        let set = KvCall { handle, under, key: key.to_owned(), ..KvCall::default() };
        client.call::<KvWritten>(method::KV_SET, &set).unwrap();
    }
    let mut count = |under: Vec<String>| client.call::<KvCount>(method::KV_COUNT, &KvBranch { handle, under }).unwrap();
    assert_eq!(count(vec![]).value, 1, "the root's own key, not its branch's");
    assert_eq!(count(vec!["42".to_owned()]).value, 2);
    assert_eq!(count(vec!["7".to_owned()]).value, 0);
}
