//! One hostile run of replication, by hand: five nodes, a writer on each of
//! four, and chaos that restarts nodes and the witness, moves a database's
//! home and cuts a synchronous home from its witness, after which every write
//! answered is checked to be on every node, no two homes of a file to have
//! taken writes at once, and every copy to be its home page for page.
//!
//! ```text
//! cargo test -p tinystore --release --all-features --lib replication::stress_tests -- --ignored --nocapture
//! ```
//!
//! `TINYSTORE_STRESS_SEED` gives the chaos's order and pauses again; the run
//! prints the seed it took.
//!
//! ```text
//!        witness ── judges A, B, C, D
//!   A  catalog, home of kv.db, synchronous with B
//!   ├── B  copy of kv.db, home of sql/tenant.db
//!   │    └── C  copy of copies
//!   └── D  copy of kv.db, takes sql/tenant.db straight from its home
//! ```

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use super::*;
use crate::{ErrorKind, Options, Store};

const NOTES: [(&str, &str); 1] =
    [("0001_notes.sql", "create table notes (id integer primary key, body text not null);")];
const KV: &str = "kv.db";
const TENANT: &str = "sql/tenant.db";
const NAMES: [&str; 5] = ["witness", "a", "b", "c", "d"];
const W: usize = 0;
const A: usize = 1;
const B: usize = 2;
const C: usize = 3;
const D: usize = 4;

/// A node of the run: the options it opens with again after each restart.
struct Peer {
    name: &'static str,
    dir: tempfile::TempDir,
    node: Node,
    store: RwLock<Option<Store>>,
}

impl Peer {
    fn open(&self) -> Store {
        let options = Options { node: Some(self.node.clone()), background: false, ..Options::default() };
        Store::open(self.dir.path(), options).unwrap_or_else(|error| panic!("{}: {error}", self.name))
    }

    fn store(&self) -> Option<Store> {
        self.store.read().unwrap().clone()
    }

    fn running(&self) -> Store {
        self.store().unwrap_or_else(|| panic!("{} is stopped", self.name))
    }

    fn id(&self) -> String {
        self.running().node().unwrap().id.clone()
    }

    fn restart(&self) {
        if let Some(store) = self.store.write().unwrap().take() {
            let closing = Instant::now();
            store.close().unwrap_or_else(|error| panic!("{} closing: {error}", self.name));
            println!("{} closed in {:?}", self.name, closing.elapsed());
        }
        let opening = Instant::now();
        *self.store.write().unwrap() = Some(self.open());
        println!("{} opened in {:?}", self.name, opening.elapsed());
    }

    /// Whether this node writes `file` itself now.
    fn writes(&self, file: &str) -> bool {
        self.home_epoch(file).is_some()
    }

    /// The epoch this node writes `file` at itself now; none while it does not.
    fn home_epoch(&self, file: &str) -> Option<u64> {
        let store = self.store()?;
        let node = store.node().unwrap();
        let home = node.copy(file).is_none() && !super::copy::is_copy(&node.log_dir(file));
        home.then(|| node.epoch_written(file)).flatten()
    }
}

/// What a write was, and how it was answered.
#[derive(Clone, Debug)]
struct Done {
    peer: usize,
    what: What,
    key: String,
    answer: Answer,
    at: Instant,
    /// The epoch the writer's node wrote the file at itself, its home at
    /// that epoch before and after the write; none for a write it passed on.
    home: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum What {
    Set,
    Add,
    Insert,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Answer {
    Ok,
    /// Committed at a synchronous home that no copy held in time.
    Local,
    /// Its commit may or may not have happened.
    Unknown,
    Refused,
}

impl Answer {
    fn of<T>(outcome: &crate::Result<T>) -> Answer {
        match outcome {
            Ok(_) => Answer::Ok,
            Err(error) if error.kind() == ErrorKind::CommittedLocally => Answer::Local,
            Err(error) if error.kind() == ErrorKind::OutcomeUnknown => Answer::Unknown,
            Err(_) => Answer::Refused,
        }
    }
}

/// xorshift64*, enough to order chaos again from a seed.
struct Dice(u64);

impl Dice {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn pause(&mut self) -> Duration {
        Duration::from_millis(1500 + self.next() % 3000)
    }

    fn shuffle<T>(&mut self, items: &mut [T]) {
        for at in (1..items.len()).rev() {
            items.swap(at, (self.next() % (at as u64 + 1)) as usize);
        }
    }
}

fn free_address() -> String {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().to_string()
}

fn eventually(what: &str, within: Duration, mut holds: impl FnMut() -> bool) {
    let deadline = Instant::now() + within;
    while !holds() {
        assert!(Instant::now() < deadline, "{what}: not within {within:?}");
        thread::sleep(Duration::from_millis(50));
    }
}

fn say(began: Instant, what: impl std::fmt::Display) {
    println!("{:>7.1}s  {what}", began.elapsed().as_secs_f64());
}

fn network() -> Vec<Arc<Peer>> {
    let addresses: Vec<String> = (0..5).map(|_| free_address()).collect();
    let failover = Some(addresses[W].clone());
    let nodes = [
        Node { serve: Some(addresses[W].clone()), witness: true, ..Node::default() },
        Node {
            serve: Some(addresses[A].clone()),
            catalog: true,
            failover: failover.clone(),
            synchronous_with: vec![addresses[B].clone()],
            ..Node::default()
        },
        Node {
            serve: Some(addresses[B].clone()),
            from: vec![addresses[A].clone()],
            failover: failover.clone(),
            ..Node::default()
        },
        Node {
            serve: Some(addresses[C].clone()),
            from: vec![addresses[B].clone(), addresses[A].clone()],
            failover: failover.clone(),
            ..Node::default()
        },
        Node { serve: Some(addresses[D].clone()), from: vec![addresses[A].clone()], failover, ..Node::default() },
    ];
    nodes
        .into_iter()
        .zip(NAMES)
        .map(|(node, name)| Arc::new(Peer { name, dir: tempfile::tempdir().unwrap(), node, store: RwLock::new(None) }))
        .collect()
}

/// Starts the network: the witness, the catalog's home with kv.db, B with
/// the tenant's database, then the copies, each holding what it copies.
fn start(peers: &[Arc<Peer>]) {
    let within = Duration::from_secs(60);
    peers[W].restart();
    peers[A].restart();
    let a = peers[A].running();
    // no copy holds it yet, which a synchronous home says
    let _ = a.bucket::<String>("sessions").open().unwrap().set("started", &"a".to_owned());
    a.counters("views").open().unwrap();
    peers[B].restart();
    let b = peers[B].running();
    eventually("b copies kv.db", within, || b.node().unwrap().copy(KV).is_some());
    let db = b.database("tenant").migrations(NOTES).open().unwrap();
    db.exec(crate::sql!("insert into notes (body) values ('started')")).unwrap();
    for peer in [C, D] {
        peers[peer].restart();
        let store = peers[peer].running();
        eventually(&format!("{} copies kv.db", peers[peer].name), within, || store.node().unwrap().copy(KV).is_some());
        store.database("tenant").migrations(NOTES).open().unwrap();
        eventually(&format!("{} copies the tenant", peers[peer].name), within, || {
            store.node().unwrap().copy(TENANT).is_some()
        });
    }
}

/// Writes on `peer` until `stop`: a set each turn, a counter's add every
/// third and a row of the tenant every fifth, each answer kept.
fn write(peer: Arc<Peer>, at: usize, stop: Arc<AtomicBool>, done: Arc<Mutex<Vec<Done>>>) {
    let value = format!("written on {}", peer.name);
    for turn in 0u64.. {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let Some(store) = peer.store() else {
            thread::sleep(Duration::from_millis(20));
            continue;
        };
        let key = format!("{}-{turn}", peer.name);
        let mut kept = Vec::new();
        let mut keep = |what: What, file: &str, write: &dyn Fn() -> crate::Result<()>| {
            // a write the node made as the file's home: its home at one epoch before and after
            let was = peer.home_epoch(file);
            let answer = Answer::of(&write());
            let home = was.filter(|_| peer.home_epoch(file) == was);
            kept.push(Done { peer: at, what, key: key.clone(), answer, at: Instant::now(), home });
        };
        keep(What::Set, KV, &|| store.bucket::<String>("sessions").open().and_then(|bucket| bucket.set(&key, &value)));
        if turn % 3 == 0 {
            keep(What::Add, KV, &|| store.counters("views").open().and_then(|views| views.add("all", 1)).map(drop));
        }
        if turn % 5 == 0 {
            let insert = || crate::sql!("insert into notes (body) values (?)", key.clone());
            keep(What::Insert, TENANT, &|| {
                store.database("tenant").migrations(NOTES).open().and_then(|db| db.exec(insert())).map(drop)
            });
        }
        done.lock().unwrap().extend(kept);
        thread::sleep(Duration::from_millis(15));
    }
}

/// Moves the tenant's home to `to`, trying again while its copy catches up.
fn move_tenant(peers: &[Arc<Peer>], to: usize, began: Instant) {
    // a failover may have made it the home already
    if peers[to].writes(TENANT) {
        return say(began, format!("{} writes the tenant already", peers[to].name));
    }
    for _ in 0..20 {
        let Some(store) = peers[to].store() else { return };
        match crate::replication::move_home(&store, TENANT) {
            Ok(moved) => {
                say(began, format!("moved the tenant to {} at epoch {}", peers[to].name, moved.epoch));
                return;
            }
            Err(error) => say(began, format!("a move to {} failed: {error}", peers[to].name)),
        }
        thread::sleep(Duration::from_secs(1));
    }
    panic!("the tenant never moved to {}", peers[to].name);
}

/// Cuts kv.db's home `from` off its witness until `to` writes it, and gives
/// back when that happened.
fn fail_over(peers: &[Arc<Peer>], from: usize, to: usize, began: Instant) -> Instant {
    let home = peers[from].id();
    let witness = peers[W].running();
    witness.node().unwrap().witness.as_ref().unwrap().cut(&home);
    say(began, format!("cut {} off the witness", peers[from].name));
    eventually(&format!("{} takes kv.db over", peers[to].name), Duration::from_secs(60), || peers[to].writes(KV));
    let taken = Instant::now();
    say(began, format!("{} writes kv.db", peers[to].name));
    peers[W].running().node().unwrap().witness.as_ref().unwrap().heal(&home);
    eventually(&format!("{} becomes a copy", peers[from].name), Duration::from_secs(60), || {
        peers[from].store().is_some_and(|store| store.node().unwrap().copy(KV).is_some())
    });
    taken
}

#[derive(Clone, Copy, Debug)]
enum Chaos {
    Restart(usize),
    Move(usize),
}

fn run_chaos(peers: &[Arc<Peer>], chaos: &[Chaos], dice: &mut Dice, began: Instant) {
    for event in chaos {
        thread::sleep(dice.pause());
        match *event {
            Chaos::Restart(peer) => {
                say(began, format!("restart {}", peers[peer].name));
                peers[peer].restart();
            }
            Chaos::Move(to) => move_tenant(peers, to, began),
        }
    }
}

/// Waits until every copy of `file` stands where its home does, and says
/// where each stood when one never did.
fn converged(peers: &[Arc<Peer>], file: &str, home: usize) {
    let home_store = peers[home].running();
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let at = home_store.node().unwrap().point_of(file);
        let behind: Vec<String> = peers[A..]
            .iter()
            .enumerate()
            .filter(|(index, _)| index + A != home)
            .filter_map(|(_, peer)| {
                let held = peer.running().node().unwrap().copy(file).map(|copy| copy.point());
                // a move with no commit since names the same last entry at its new epoch
                let same = |held: &super::entry::Point| {
                    at.is_some_and(|at| (at.sequence, at.check) == (held.sequence, held.check))
                };
                (!held.as_ref().is_some_and(same)).then(|| match held {
                    Some(point) => format!("{} at entry {} epoch {}", peer.name, point.sequence, point.epoch),
                    None => format!("{} holds no whole copy", peer.name),
                })
            })
            .collect();
        if behind.is_empty() && at.is_some() {
            return;
        }
        let home_at = at.map_or("nowhere".to_owned(), |at| format!("entry {} epoch {}", at.sequence, at.epoch));
        assert!(
            Instant::now() < deadline,
            "{file}: its home {} stands at {home_at}, and {}",
            peers[home].name,
            behind.join(", ")
        );
        thread::sleep(Duration::from_millis(100));
    }
}

/// The node that writes `file` now.
fn home_of(peers: &[Arc<Peer>], file: &str) -> usize {
    (A..=D).find(|&peer| peers[peer].writes(file)).unwrap_or_else(|| panic!("no node writes {file}"))
}

/// Checks that no two nodes wrote `file` as its home at one epoch, and that
/// no write as home at an epoch came after one at a later epoch: in the order
/// they were answered, the epochs of writes made as home only grow, each the
/// home of one node.
fn no_two_homes(peers: &[Arc<Peer>], done: &[Done], what: &[What], file: &str) {
    let mut writes: Vec<&Done> = done
        .iter()
        .filter(|done| done.home.is_some() && done.answer == Answer::Ok && what.contains(&done.what))
        .collect();
    writes.sort_by_key(|done| done.at);
    let mut homes: Vec<(u64, usize)> = Vec::new();
    for write in writes {
        let epoch = write.home.expect("a write made as home");
        match homes.last() {
            Some(&(last, peer)) if last == epoch => {
                assert_eq!(
                    peer, write.peer,
                    "{file}: {} and {} both wrote it as home at epoch {epoch}",
                    peers[peer].name, peers[write.peer].name
                );
            }
            Some(&(last, peer)) => {
                assert!(
                    epoch > last,
                    "{file}: {} wrote it as home at epoch {epoch} after {} did at epoch {last}",
                    peers[write.peer].name,
                    peers[peer].name
                );
                homes.push((epoch, write.peer));
            }
            None => homes.push((epoch, write.peer)),
        }
    }
    let line: Vec<String> = homes.iter().map(|&(epoch, peer)| format!("{} at {epoch}", peers[peer].name)).collect();
    println!("{file} written as home by {}", line.join(", then "));
}

#[test]
#[ignore = "a run of minutes, by hand"]
fn every_answered_write_survives_restarts_moves_and_a_failover_and_every_copy_is_its_home() {
    let seed = std::env::var("TINYSTORE_STRESS_SEED").ok().and_then(|seed| seed.parse().ok()).unwrap_or_else(|| {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64 | 1
    });
    println!("TINYSTORE_STRESS_SEED={seed}");
    // the store's own lines, each of a node's threads naming its node; TINYSTORE_STRESS_LOG=debug for every turn
    let debug = std::env::var("TINYSTORE_STRESS_LOG").is_ok_and(|level| level == "debug");
    let level = if debug { tracing::Level::DEBUG } else { tracing::Level::INFO };
    let console = crate::console::logger("stress").console(crate::console::Format::Pretty).level(level).layer();
    let _ = tracing_subscriber::util::SubscriberInitExt::try_init(tracing_subscriber::layer::SubscriberExt::with(
        tracing_subscriber::registry(),
        console,
    ));
    let mut dice = Dice(seed);
    let began = Instant::now();
    let peers = network();
    start(&peers);
    say(began, "started");

    let (stop, done) = (Arc::new(AtomicBool::new(false)), Arc::new(Mutex::new(Vec::new())));
    let writers: Vec<_> = (A..=D)
        .map(|at| {
            let (peer, stop, done) = (Arc::clone(&peers[at]), Arc::clone(&stop), Arc::clone(&done));
            thread::spawn(move || write(peer, at, stop, done))
        })
        .collect();

    // A writes kv.db and waits for B to hold each commit; the tenant moves away and back
    let mut first = vec![Chaos::Restart(C), Chaos::Restart(D), Chaos::Restart(W), Chaos::Move(D)];
    dice.shuffle(&mut first);
    run_chaos(&peers, &first, &mut dice, began);
    let taken = fail_over(&peers, A, B, began);
    let mut then = vec![Chaos::Restart(C), Chaos::Restart(A), Chaos::Restart(W), Chaos::Move(B)];
    dice.shuffle(&mut then);
    run_chaos(&peers, &then, &mut dice, began);
    thread::sleep(dice.pause());
    stop.store(true, Ordering::SeqCst);
    for writer in writers {
        writer.join().unwrap();
    }
    say(began, "writers stopped");

    let done = done.lock().unwrap().clone();
    check(&peers, &done, taken, began);
    for peer in peers.iter().rev() {
        if let Some(store) = peer.store.write().unwrap().take() {
            store.close().unwrap();
        }
    }
}

fn check(peers: &[Arc<Peer>], done: &[Done], taken: Instant, began: Instant) {
    let (kv_home, tenant_home) = (home_of(peers, KV), home_of(peers, TENANT));
    assert_eq!(kv_home, B, "kv.db's home is the copy it failed over to");
    converged(peers, KV, kv_home);
    converged(peers, TENANT, tenant_home);
    say(began, "converged");
    let count = |what: What, answer: Answer| done.iter().filter(|d| d.what == what && d.answer == answer).count();
    for what in [What::Set, What::Add, What::Insert] {
        let line: Vec<String> = [Answer::Ok, Answer::Local, Answer::Unknown, Answer::Refused]
            .iter()
            .map(|&answer| format!("{answer:?} {}", count(what, answer)))
            .collect();
        say(began, format!("{what:?}: {}", line.join(", ")));
    }

    // every set answered is on every node, those before the failover among them
    let answered: Vec<&Done> = done.iter().filter(|d| d.what == What::Set && d.answer == Answer::Ok).collect();
    assert!(answered.iter().any(|d| d.at < taken), "some sets were answered before the failover");
    for peer in &peers[A..] {
        let sessions = peer.running().bucket::<String>("sessions").open().unwrap();
        let missing: Vec<&str> =
            answered.iter().filter(|d| sessions.get(&d.key).unwrap().is_none()).map(|d| d.key.as_str()).collect();
        assert!(missing.is_empty(), "{}: {} sets answered are missing: {:?}", peer.name, missing.len(), missing);
    }

    // each add answered counted once, and none that was refused
    let views = peers[kv_home].running().counters("views").open().unwrap().get("all").unwrap();
    let (ok, maybe) =
        (count(What::Add, Answer::Ok), count(What::Add, Answer::Local) + count(What::Add, Answer::Unknown));
    assert!(
        views >= ok as i64 && views <= (ok + maybe) as i64,
        "{views} counted of {ok} adds answered, {maybe} uncertain"
    );

    // every row answered is in the tenant's database on every node
    let rows: Vec<&Done> = done.iter().filter(|d| d.what == What::Insert && d.answer == Answer::Ok).collect();
    for peer in &peers[A..] {
        let db = peer.running().database("tenant").migrations(NOTES).open().unwrap();
        let bodies: std::collections::HashSet<String> =
            db.all::<String>(crate::sql!("select body from notes")).unwrap().into_iter().collect();
        let missing = rows.iter().filter(|d| !bodies.contains(&d.key)).count();
        assert_eq!(missing, 0, "{}: rows answered missing from the tenant", peer.name);
    }

    // no two homes took writes of a file at once
    no_two_homes(peers, done, &[What::Set, What::Add], KV);
    no_two_homes(peers, done, &[What::Insert], TENANT);

    // every copy is its home page for page, and every node writes again
    for (file, home) in [(KV, kv_home), (TENANT, tenant_home)] {
        for (index, peer) in peers[A..].iter().enumerate() {
            if index + A != home {
                let sequence = crate::replication::verify(&peer.running(), file)
                    .unwrap_or_else(|error| panic!("{}: {file}: {error}", peer.name));
                say(began, format!("{} holds {file} as its home does at entry {sequence}", peer.name));
            }
        }
    }
    for peer in &peers[A..] {
        let store = peer.running();
        store.bucket::<String>("sessions").open().unwrap().set("last", &peer.name.to_owned()).unwrap();
        store
            .database("tenant")
            .migrations(NOTES)
            .open()
            .unwrap()
            .exec(crate::sql!("insert into notes (body) values ('last')"))
            .unwrap();
    }
    say(began, "every write answered survived");
}
