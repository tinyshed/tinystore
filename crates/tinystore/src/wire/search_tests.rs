use crate::pipe::Capability;
use crate::pipe::fixture::Client;
use crate::wire::codec::Cell;
use crate::wire::protocol::{
    Empty, Handle, SearchDeleted, SearchDocument, SearchDropped, SearchField, SearchFound, SearchGot, SearchKey,
    SearchListed, SearchName, SearchOpen, SearchQuery, SearchSet, SearchSetMany, SearchStatus, SqlDone, SqlMigration,
    SqlOpen, SqlStatement, method,
};
use crate::{Options, Store};

fn open(client: &mut Client, name: &str) -> u64 {
    let asked = SearchOpen {
        name: name.to_owned(),
        fields: vec![SearchField { path: "title".to_owned(), weight: 2.0 }],
        filters: vec!["category".to_owned(), "price".to_owned()],
        dimensions: Some(3),
        ..SearchOpen::default()
    };
    client.call::<Handle>(method::SEARCH_OPEN, &asked).unwrap().handle
}

fn bytes(vector: &[f32]) -> Vec<u8> {
    vector.iter().flat_map(|value| value.to_le_bytes()).collect()
}

fn set(client: &mut Client, handle: u64, (id, value): (&str, &str), vector: &[f32]) {
    let asked =
        SearchSet { handle, id: id.to_owned(), value: value.to_owned(), vector: Some(bytes(vector)), no_vector: false };
    client.call::<Empty>(method::SEARCH_SET, &asked).unwrap();
}

fn query(handle: u64, text: &str) -> SearchQuery {
    SearchQuery { handle, text: text.to_owned(), ..SearchQuery::default() }
}

fn ids(found: &SearchFound) -> Vec<&str> {
    found.hits.iter().map(|hit| hit.id.as_str()).collect()
}

#[test]
fn an_index_sets_reads_searches_and_deletes_documents_over_the_wire() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let handle = open(&mut client, "products");
    set(&mut client, handle, ("a", r#"{"title":"red shoes","category":"shoes","price":50}"#), &[1.0, 0.0, 0.0]);
    set(&mut client, handle, ("b", r#"{"title":"blue shoes","category":"boots","price":150}"#), &[0.0, 1.0, 0.0]);
    let many = SearchSetMany {
        handle,
        documents: vec![SearchDocument {
            id: "c".to_owned(),
            value: r#"{"title":"red hat"}"#.to_owned(),
            vector: None,
            no_vector: true,
        }],
    };
    client.call::<Empty>(method::SEARCH_SET_MANY, &many).unwrap();
    let found: SearchFound = client.call(method::SEARCH_SEARCH, &query(handle, "red")).unwrap();
    assert_eq!(ids(&found), ["a", "c"], "of equal scores, the document set first");
    assert_eq!(found.hits[0].value, r#"{"category":"shoes","price":50,"title":"red shoes"}"#);
    let by_vector = SearchQuery { vector: Some(bytes(&[0.0, 1.0, 0.0])), limit: Some(1), ..query(handle, "") };
    assert_eq!(ids(&client.call(method::SEARCH_SEARCH, &by_vector).unwrap()), ["b"]);
    let filtered = SearchQuery {
        r#where: Some(r#"[{"or":[{"eq":["category","boots"]},{"range":["price",{"lt":100}]}]}]"#.to_owned()),
        ..query(handle, "shoes")
    };
    assert_eq!(ids(&client.call(method::SEARCH_SEARCH, &filtered).unwrap()), ["a", "b"]);
    let got: SearchGot = client.call(method::SEARCH_GET, &SearchKey { handle, id: "c".to_owned() }).unwrap();
    assert!(got.found && got.value.as_deref() == Some(r#"{"title":"red hat"}"#));
    let deleted: SearchDeleted = client.call(method::SEARCH_DELETE, &SearchKey { handle, id: "c".to_owned() }).unwrap();
    assert!(deleted.deleted);
    let status: SearchStatus = client.call(method::SEARCH_STATUS, &Handle { handle }).unwrap();
    assert_eq!(status.state, "ready");
}

#[test]
fn a_refusal_over_the_wire_names_what_failed() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let handle = open(&mut client, "products");
    let unsaid = SearchSet { handle, id: "a".to_owned(), value: "{}".to_owned(), vector: None, no_vector: false };
    let refused = client.call::<Empty>(method::SEARCH_SET, &unsaid).unwrap_err();
    assert_eq!(refused.code, "invalid");
    assert!(refused.message.contains("keeps vectors"), "{}", refused.message);
    let ragged = SearchSet { vector: Some(vec![0; 5]), ..unsaid };
    assert_eq!(client.call::<Empty>(method::SEARCH_SET, &ragged).unwrap_err().code, "invalid");
    let where_bad = SearchQuery { r#where: Some(r#"{"near":["price",1]}"#.to_owned()), ..query(handle, "x") };
    assert_eq!(client.call::<SearchFound>(method::SEARCH_SEARCH, &where_bad).unwrap_err().code, "invalid");
    assert_eq!(client.call::<SearchStatus>(method::SEARCH_STATUS, &Handle { handle: 99 }).unwrap_err().code, "invalid");
}

#[test]
fn a_read_connection_opens_an_index_only_as_it_is_and_writes_none() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap();
    let mut writer = Client::admitted(&store, Capability::Data);
    let handle = open(&mut writer, "products");
    set(&mut writer, handle, ("a", r#"{"title":"red shoes"}"#), &[1.0, 0.0, 0.0]);
    let mut reader = Client::admitted(&store, Capability::Read);
    let asked = SearchOpen { name: "products".to_owned(), ..SearchOpen::default() };
    let read = reader.call::<Handle>(method::SEARCH_OPEN, &asked).unwrap().handle;
    let found: SearchFound = reader.call(method::SEARCH_SEARCH, &query(read, "red")).unwrap();
    assert_eq!(ids(&found), ["a"], "its settings as they are, not the empty ones asked");
    let missing = SearchOpen { name: "nothing".to_owned(), ..SearchOpen::default() };
    assert_eq!(reader.call::<Handle>(method::SEARCH_OPEN, &missing).unwrap_err().code, "invalid");
    let write = SearchSet { handle: read, id: "b".to_owned(), value: "{}".to_owned(), vector: None, no_vector: true };
    assert_eq!(reader.call::<Empty>(method::SEARCH_SET, &write).unwrap_err().code, "permission");
}

#[test]
fn an_index_is_listed_and_dropped_over_the_wire() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::open(dir.path());
    let handle = open(&mut client, "products");
    set(&mut client, handle, ("a", r#"{"title":"red shoes"}"#), &[1.0, 0.0, 0.0]);
    let listed: SearchListed = client.call(method::SEARCH_LIST, &Empty {}).unwrap();
    assert_eq!(listed.indexes.len(), 1);
    assert_eq!((listed.indexes[0].name.as_str(), listed.indexes[0].documents), ("products", 1));
    assert!(listed.indexes[0].opened.is_some());
    let dropped: SearchDropped =
        client.call(method::SEARCH_DROP_INDEX, &SearchName { name: "products".to_owned(), database: None }).unwrap();
    assert!(dropped.dropped);
    assert_eq!(client.call::<SearchFound>(method::SEARCH_SEARCH, &query(handle, "red")).unwrap_err().code, "invalid");
}

#[test]
fn a_table_s_index_opens_in_its_database_and_its_hits_carry_their_rows() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap();
    let mut client = Client::admitted(&store, Capability::Data);
    let articles =
        "create table articles (id integer primary key, title text not null, done integer not null default 0)";
    let migrations = Some(vec![SqlMigration { name: "0001_articles.sql".to_owned(), sql: articles.to_owned() }]);
    let open = SqlOpen { name: "app".to_owned(), migrations, durability: None };
    let database = client.call::<Handle>(method::SQL_OPEN, &open).unwrap().handle;
    let asked = SearchOpen {
        name: "articles_search".to_owned(),
        fields: vec![SearchField { path: "title".to_owned(), weight: 1.0 }],
        filters: vec!["done".to_owned()],
        database: Some(database),
        table: Some("articles".to_owned()),
        ..SearchOpen::default()
    };
    let handle = client.call::<Handle>(method::SEARCH_OPEN, &asked).unwrap().handle;
    let insert = SqlStatement {
        handle: database,
        text: "insert into articles (id, title, done) values (7, 'rye bread', 1)".to_owned(),
        values: Vec::new(),
    };
    client.call::<SqlDone>(method::SQL_EXEC, &insert).unwrap();
    let filtered = SearchQuery { r#where: Some(r#"[{"eq":["done",true]}]"#.to_owned()), ..query(handle, "bread") };
    let found: SearchFound = client.call(method::SEARCH_SEARCH, &filtered).unwrap();
    assert_eq!(ids(&found), ["7"]);
    assert_eq!(found.columns.as_deref(), Some(&["id".to_owned(), "title".to_owned(), "done".to_owned()][..]));
    let row = [Cell::Int(7), Cell::Str("rye bread".to_owned()), Cell::Int(1)];
    assert_eq!(found.hits[0].row.as_deref(), Some(&row[..]));
    let set = SearchSet { handle, id: "8".to_owned(), value: "{}".to_owned(), vector: None, no_vector: true };
    let refused = client.call::<Empty>(method::SEARCH_SET, &set).unwrap_err();
    assert!(refused.message.contains("written to the table"), "{}", refused.message);
    let named = SearchName { name: "articles_search".to_owned(), database: Some(database) };
    assert!(client.call::<SearchDropped>(method::SEARCH_DROP_INDEX, &named).unwrap().dropped);
}
