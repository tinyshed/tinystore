use super::*;
use crate::ErrorKind;
use crate::engine::Host;

fn opened(dir: &tempfile::TempDir) -> Store {
    Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap()
}

#[tokio::test]
async fn a_close_awaited_closes_the_store_and_lets_go_of_its_directory() {
    let dir = tempfile::tempdir().unwrap();
    let store = opened(&dir);
    // spawned, so that the future is Send and 'static, as a program's runtime asks
    tokio::spawn(store.close()).await.unwrap().unwrap();
    let refused = store.blocking().claim("kv.db").unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Closed, "{refused}");
    store.close().await.unwrap();
    opened(&dir).close().await.unwrap();
}

#[cfg(feature = "kv")]
#[tokio::test]
async fn from_and_blocking_cross_between_the_two_apis_on_one_store() {
    let dir = tempfile::tempdir().unwrap();
    let blocking = crate::Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap();
    let store = Store::from(blocking.clone());
    let again = Store::open(dir.path(), Options::default()).unwrap_err();
    assert_eq!(again.kind(), ErrorKind::InUse, "the aio store is the same store, not a second: {again}");
    assert_eq!(store.blocking().dir(), blocking.dir());

    let sessions = blocking.bucket::<String>("sessions").open().unwrap();
    sessions.set("42", &"written through the blocking API".to_owned()).unwrap();
    let read = store.blocking().bucket::<String>("sessions").open().unwrap();
    assert_eq!(read.get("42").unwrap().as_deref(), Some("written through the blocking API"));

    store.close().await.unwrap();
    let closed = sessions.get("42").unwrap_err();
    assert_eq!(closed.kind(), ErrorKind::Closed, "an aio close closes the blocking store too: {closed}");
}

#[cfg(feature = "kv")]
#[tokio::test]
async fn a_transaction_awaited_runs_its_closure_on_the_pool_and_commits_what_it_returns() {
    let dir = tempfile::tempdir().unwrap();
    let store = opened(&dir);
    let stock = store.blocking().bucket::<i64>("stock").open().unwrap();
    stock.set("sku-1", &1).unwrap();

    let taking = stock.clone();
    let placed = store
        .tx(move |tx| -> Result<(bool, Option<String>)> {
            let ran_on = std::thread::current().name().map(str::to_owned);
            let left = tx.with(&taking).get("sku-1")?.unwrap_or(0);
            if left < 1 {
                return Ok((false, ran_on));
            }
            tx.with(&taking).set("sku-1", &(left - 1))?;
            Ok((true, ran_on))
        })
        .await
        .unwrap();
    assert_eq!(placed, (true, Some(crate::pool::THREAD.to_owned())), "the closure ran on the store's pool");
    assert_eq!(stock.get("sku-1").unwrap(), Some(0));

    let failing = stock.clone();
    let refused = store
        .tx(move |tx| -> Result<()> {
            tx.with(&failing).set("sku-1", &100)?;
            Err(Error::invalid("the order was refused"))
        })
        .await
        .unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Invalid);
    assert_eq!(stock.get("sku-1").unwrap(), Some(0), "a closure that fails rolls back");
    store.close().await.unwrap();
}

#[cfg(feature = "kv")]
#[tokio::test]
async fn a_transaction_that_panics_rolls_back_and_resumes_the_panic_at_the_await() {
    let dir = tempfile::tempdir().unwrap();
    let store = opened(&dir);
    let stock = store.blocking().bucket::<i64>("stock").open().unwrap();
    let panicking = stock.clone();
    let tx = store.tx(move |tx| -> Result<()> {
        tx.with(&panicking).set("sku-1", &5)?;
        panic!("the closure panicked");
    });
    let resumed = tokio::spawn(tx).await.unwrap_err();
    assert!(resumed.is_panic());
    assert_eq!(stock.get("sku-1").unwrap(), None, "the panic rolled the transaction back");
    stock.set("sku-1", &1).unwrap();
    store.close().await.unwrap();
}

#[cfg(feature = "kv")]
#[tokio::test]
async fn a_call_on_a_store_that_closed_answers_closed_rather_than_wait() {
    let dir = tempfile::tempdir().unwrap();
    let store = opened(&dir);
    let later = store.clone();
    store.close().await.unwrap();
    let closed = later.tx(|_| -> Result<()> { Ok(()) }).await.unwrap_err();
    assert_eq!(closed.kind(), ErrorKind::Closed, "{closed}");

    // one made from a store closed already has a pool that runs nothing
    let made_after = Store::from(store.blocking().clone());
    let closed = made_after.tx(|_| -> Result<()> { Ok(()) }).await.unwrap_err();
    assert_eq!(closed.kind(), ErrorKind::Closed, "{closed}");
}

// contract: DUR-07
#[cfg(feature = "kv")]
#[test]
fn an_awaited_call_from_inside_a_transaction_is_refused_rather_than_wait_for_itself() {
    let dir = tempfile::tempdir().unwrap();
    let store = opened(&dir);
    let stock = store.blocking().bucket::<i64>("stock").open().unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let nested = store.blocking().tx(|tx| -> Result<ErrorKind> {
        tx.with(&stock).set("sku-1", &1)?;
        let refused = runtime.block_on(store.tx(|_| -> Result<()> { Ok(()) })).unwrap_err();
        Ok(refused.kind())
    });
    assert_eq!(nested.unwrap(), ErrorKind::Invalid);
    assert_eq!(stock.get("sku-1").unwrap(), Some(1), "the transaction around it went on and committed");
    store.blocking().close().unwrap();
}
