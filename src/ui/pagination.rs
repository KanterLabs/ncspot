use crate::library::Library;
use crate::traits::ListItem;
use log::debug;
use std::sync::{Arc, RwLock};

pub struct ApiPage<I> {
    pub offset: u32,
    pub total: u32,
    pub items: Vec<I>,
}
pub type FetchPageFn<I> = dyn Fn(u32) -> Option<ApiPage<I>> + Send + Sync;
pub struct ApiResult<I> {
    offset: Arc<RwLock<u32>>,
    limit: u32,
    pub total: u32,
    pub items: Arc<RwLock<Vec<I>>>,
    fetch_page: Arc<FetchPageFn<I>>,
    /// Whether the first page could not be fetched. Without this an empty result
    /// means both "nothing there" and "never found out", and a caller that syncs
    /// against it would take the second for the first and delete everything.
    failed: bool,
}

impl<I: ListItem + Clone> ApiResult<I> {
    pub fn new(limit: u32, fetch_page: Arc<FetchPageFn<I>>) -> Self {
        let items = Arc::new(RwLock::new(Vec::new()));
        if let Some(first_page) = fetch_page(0) {
            debug!(
                "fetched first page, items: {}, total: {}",
                first_page.items.len(),
                first_page.total
            );
            items.write().unwrap().extend(first_page.items);
            Self {
                offset: Arc::new(RwLock::new(first_page.offset)),
                limit,
                total: first_page.total,
                items,
                fetch_page: fetch_page.clone(),
                failed: false,
            }
        } else {
            Self {
                offset: Arc::new(RwLock::new(0)),
                limit,
                total: 0,
                items,
                fetch_page: fetch_page.clone(),
                failed: true,
            }
        }
    }

    /// Whether the first page could not be fetched, as opposed to coming back empty.
    pub fn failed(&self) -> bool {
        self.failed
    }

    /// Move the items fetched so far into `store`, and have later pages land there too.
    ///
    /// This lets a view hand its own list to a result that was fetched after the view was built,
    /// which is how the fetch is kept off the thread that draws.
    pub fn into_store(self, store: Arc<RwLock<Vec<I>>>) -> Self {
        store
            .write()
            .unwrap()
            .extend(self.items.read().unwrap().iter().cloned());

        Self {
            items: store,
            ..self
        }
    }

    fn offset(&self) -> u32 {
        *self.offset.read().unwrap()
    }

    pub fn at_end(&self) -> bool {
        (self.offset() + self.limit) >= self.total
    }

    pub fn apply_pagination(self, pagination: &Pagination<I>) {
        let total = self.total as usize;
        let fetched_items = self.items.read().unwrap().len();
        pagination.set(
            fetched_items,
            total,
            Box::new(move |_| {
                self.next();
            }),
        )
    }

    pub fn next(&self) -> Option<Vec<I>> {
        let offset = self.offset() + self.limit;
        debug!("fetching next page at offset {offset}");
        if !self.at_end() {
            if let Some(next_page) = (self.fetch_page)(offset) {
                *self.offset.write().unwrap() = next_page.offset;
                self.items.write().unwrap().extend(next_page.items.clone());
                Some(next_page.items)
            } else {
                None
            }
        } else {
            debug!("paginator is at end");
            None
        }
    }
}

pub type Paginator<I> = Box<dyn Fn(Arc<RwLock<Vec<I>>>) + Send + Sync>;

/// Manages the loading of ListItems, to increase performance and decrease
/// memory usage.
///
/// `loaded_content`: The amount of currently loaded items
/// `max_content`: The maximum amount of items
/// `callback`: TODO: document
/// `busy`: TODO: document
#[derive(Clone)]
pub struct Pagination<I: ListItem> {
    loaded_content: Arc<RwLock<usize>>,
    max_content: Arc<RwLock<Option<usize>>>,
    callback: Arc<RwLock<Option<Paginator<I>>>>,
    busy: Arc<RwLock<bool>>,
}

impl<I: ListItem> Default for Pagination<I> {
    fn default() -> Self {
        Self {
            loaded_content: Arc::new(RwLock::new(0)),
            max_content: Arc::new(RwLock::new(None)),
            callback: Arc::new(RwLock::new(None)),
            busy: Arc::new(RwLock::new(false)),
        }
    }
}

impl<I: ListItem + Clone> Pagination<I> {
    pub fn clear(&mut self) {
        *self.max_content.write().unwrap() = None;
        *self.callback.write().unwrap() = None;
    }
    pub fn set(&self, loaded_content: usize, max_content: usize, callback: Paginator<I>) {
        *self.loaded_content.write().unwrap() = loaded_content;
        *self.max_content.write().unwrap() = Some(max_content);
        *self.callback.write().unwrap() = Some(callback);
    }

    pub fn loaded_content(&self) -> usize {
        *self.loaded_content.read().unwrap()
    }

    pub fn max_content(&self) -> Option<usize> {
        *self.max_content.read().unwrap()
    }

    /// Mark a page as in flight, for tests of views that draw the busy state.
    #[cfg(test)]
    pub fn set_busy_for_test(&self, busy: bool) {
        *self.busy.write().unwrap() = busy;
    }

    /// Whether a page is being fetched right now, which a list draws so that
    /// scrolling to the end doesn't look like the app has stopped responding.
    pub fn is_busy(&self) -> bool {
        *self.busy.read().unwrap()
    }

    pub fn call(&self, content: &Arc<RwLock<Vec<I>>>, library: Arc<Library>) {
        let pagination = self.clone();
        let content = content.clone();
        if !self.is_busy() {
            *self.busy.write().unwrap() = true;
            std::thread::spawn(move || {
                let cb = pagination.callback.read().unwrap();
                if let Some(ref cb) = *cb {
                    debug!("calling paginator!");
                    cb(content.clone());
                    *pagination.loaded_content.write().unwrap() = content.read().unwrap().len();
                    *pagination.busy.write().unwrap() = false;
                    library.trigger_redraw();
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::track::Track;

    fn track_page(offset: u32, total: u32, items: Vec<Track>) -> ApiPage<Track> {
        ApiPage {
            offset,
            total,
            items,
        }
    }

    #[test]
    fn an_empty_result_is_not_a_failed_one() {
        let result = ApiResult::new(50, Arc::new(move |_| Some(track_page(0, 0, Vec::new()))));

        assert!(
            !result.failed(),
            "an account with nothing saved answered the question"
        );
        assert_eq!(result.total, 0);
    }

    #[test]
    fn a_result_that_never_answered_is_a_failed_one() {
        let result: ApiResult<Track> = ApiResult::new(50, Arc::new(|_| None));

        assert!(
            result.failed(),
            "a caller syncing against this would delete everything it has"
        );
    }
}
