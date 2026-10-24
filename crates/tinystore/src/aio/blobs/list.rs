//! A folder's files, awaited: a page of them, every page as a `Stream`, what
//! they count, and a clear of them all.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll, ready};

use futures_core::Stream;

use super::{Files, describe, write};
use crate::Result;
use crate::aio::run;
use crate::blobs::{FileInfo, PAGE_FILES, Page, Usage};

impl Files {
    /// A page of this folder's files and of the folders in it, in the byte
    /// order of their paths, its options before [`List::page`].
    pub fn list(&self) -> List<'_> {
        List { files: self, prefix: String::new(), after: None, limit: PAGE_FILES }
    }

    /// Every file of this folder and of the folders in it, a page at a time,
    /// holding nothing between pages: a `Stream` whose pages are read on the
    /// store's pool, each as its item is asked for.
    pub fn all(&self) -> All {
        All { files: self.clone(), after: None, page: Vec::new().into_iter(), done: false, reading: None }
    }

    /// The files of this folder and of the folders in it, and their bytes, as
    /// they are now: a count, not a bound.
    pub fn usage(&self) -> impl Future<Output = Result<Usage>> + Send + 'static {
        let files = self.blocking.clone();
        run::read(&self.pool, describe(&files, "", "a usage"), move || files.usage())
    }

    /// Removes this folder and every folder in it, at once for every reader,
    /// however many files it holds. On the files themselves it empties them.
    pub fn clear(&self) -> impl Future<Output = Result<()>> + Send + 'static {
        write(self.owned(), "a clear", |owned, done| owned.files.clear_then(done), |owned| owned.files.clear())
    }
}

/// A page of a folder's files, its options before [`List::page`], as
/// [`crate::blobs::List`] gathers them.
#[derive(Debug)]
#[must_use = "a list reads nothing until its page is awaited"]
pub struct List<'f> {
    files: &'f Files,
    prefix: String,
    after: Option<String>,
    limit: usize,
}

impl List<'_> {
    /// Only the files whose path within the folder starts with this text:
    /// `"photos/1"` finds `photos/10.jpg` too.
    pub fn prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = prefix.into();
        self
    }

    /// The page that starts past this path, as a page's `next` says it.
    pub fn after(mut self, path: impl Into<String>) -> Self {
        self.after = Some(path.into());
        self
    }

    pub fn limit(mut self, files: usize) -> Self {
        self.limit = files;
        self
    }

    pub fn page(self) -> impl Future<Output = Result<Page>> + Send + 'static {
        let List { files, prefix, after, limit } = self;
        let blocking = files.blocking.clone();
        run::read(&files.pool, describe(&blocking, "", "a page"), move || {
            let list = blocking.list().prefix(prefix).limit(limit);
            match after {
                Some(after) => list.after(after).page(),
                None => list.page(),
            }
        })
    }

    /// The page past `after` when one is given, the first otherwise.
    fn after_page(self, after: Option<String>) -> impl Future<Output = Result<Page>> + Send + 'static {
        match after {
            Some(after) => self.after(after).page(),
            None => self.page(),
        }
    }
}

/// Every file of a folder, a page at a time, as [`Files::all`] walks them:
/// each page read on the store's pool, and the page under way stopped when
/// the stream is dropped. A page that fails is the last item.
#[must_use = "streams do nothing unless polled"]
pub struct All {
    files: Files,
    after: Option<String>,
    page: std::vec::IntoIter<FileInfo>,
    done: bool,
    reading: Option<Pin<Box<dyn Future<Output = Result<Page>> + Send>>>,
}

impl std::fmt::Debug for All {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "All({:?}, after {:?})", self.files, self.after)
    }
}

impl Stream for All {
    type Item = Result<FileInfo>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Result<FileInfo>>> {
        loop {
            if let Some(file) = self.page.next() {
                return Poll::Ready(Some(Ok(file)));
            }
            if self.done {
                return Poll::Ready(None);
            }
            if self.reading.is_none() {
                let after = self.after.take();
                let page = self.files.list().after_page(after);
                self.reading = Some(Box::pin(page));
            }
            let Some(reading) = self.reading.as_mut() else { continue };
            let read = ready!(reading.as_mut().poll(context));
            self.reading = None;
            match read {
                Ok(page) => {
                    self.done = page.next.is_none();
                    self.after = page.next;
                    self.page = page.files.into_iter();
                }
                Err(error) => {
                    self.done = true;
                    return Poll::Ready(Some(Err(error)));
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "list_tests.rs"]
mod tests;
