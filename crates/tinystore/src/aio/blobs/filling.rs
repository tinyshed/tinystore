//! An awaited `get_or_create`: the blocking one's claim, its waits and its
//! fence, its function a future polled inside the call's, and every step
//! that writes or waits on no thread of the program's.

use std::fmt;
use std::future::Future;
use std::sync::Arc;

use super::{Owned, StoredFile, describe};
use crate::aio::oneshot::pending;
use crate::aio::run;
use crate::blobs::{Fill, Filling, Hand, Outcome, TIMEOUT, Waiter, failure_of};
use crate::{Error, Result};

/// The file at the call's path, or the one `work` makes: a get as a point
/// read, then the claim on the pool, a wait for the run that holds the path
/// on no thread, or `work`'s future and its write on the pool.
pub(super) async fn get_or_create<T, E, W, F>(
    owned: Owned,
    whole: String,
    mut work: W,
) -> std::result::Result<StoredFile, E>
where
    T: Into<Fill> + Send,
    E: From<Error> + fmt::Display + Send + 'static,
    W: FnMut() -> F + Send,
    F: Future<Output = std::result::Result<T, E>> + Send,
{
    let timeout = owned.options.timeout.unwrap_or(TIMEOUT);
    let mut outcome = Outcome::Hit;
    let answered = loop {
        match found(&owned).await {
            Ok(Some(file)) => break Ok(file),
            Ok(None) => {}
            Err(error) => break Err(E::from(error)),
        }
        let (told, ended) = pending(owned.what("a run under way"));
        let waiter: Waiter = Box::new(move |failure| told.send(Ok(failure)));
        let (files, at) = (owned.files.clone(), whole.clone());
        let claim = move || files.hand(&at, timeout, None, waiter).map_err(|error| files.fail(&at, error));
        let filling = match run::blocking(&owned.pool, None, owned.what("a claim"), claim).await {
            Err(error) => break Err(E::from(error)),
            Ok(Hand::There) => continue,
            Ok(Hand::Waiting) => {
                outcome = outcome.max(Outcome::Wait);
                // a waiter the store dropped as it closed answers Closed
                match ended.await {
                    Ok(Some(failure)) => break Err(E::from(failure.duplicate())),
                    Ok(None) => continue,
                    Err(error) => break Err(E::from(error)),
                }
            }
            Ok(Hand::Yours(filling)) => filling,
        };
        outcome = Outcome::Run;
        let fill = match work().await {
            Ok(fill) => fill.into(),
            Err(error) => {
                let failure = failure_of(&error).within(format!("{}: {whole:?}", owned.files.describe()));
                filling.fail(failure);
                break Err(error);
            }
        };
        if let Err(error) = land(&owned, filling, fill).await {
            break Err(E::from(error));
        }
    };
    if answered.is_err() {
        outcome = Outcome::Failed;
    }
    outcome.count(&owned.files);
    answered
}

/// The file at the path, as an awaited get finds it.
async fn found(owned: &Owned) -> Result<Option<StoredFile>> {
    let (files, path) = (owned.files.clone(), owned.path.clone());
    let opened = run::point(&owned.pool, owned.what("a get"), move || files.get(&path)).await?;
    Ok(opened.map(|file| StoredFile::new(file, Arc::clone(&owned.pool))))
}

/// Writes what the run's function gave on the pool, where a reader's bytes
/// and a file's are read; a failure is every waiting call's too. Once polled
/// the write runs to its end, and the claim is let go after it.
async fn land(owned: &Owned, filling: Filling, fill: Fill) -> Result<()> {
    let options = owned.options.clone();
    let landing = move || match filling.land(fill, &options) {
        Ok(_) => Ok(()),
        Err(error) => {
            filling.fail(error.duplicate());
            Err(error)
        }
    };
    run::blocking(&owned.pool, None, describe(&owned.files, &owned.path, "a getOrCreate's write"), landing).await
}

#[cfg(test)]
#[path = "filling_tests.rs"]
mod tests;
