use std::sync::{Arc, RwLock};
use std::thread;

use cursive::Cursive;
use cursive::view::ViewWrapper;

use crate::command::Command;
use crate::commands::CommandResult;
use crate::library::Library;
use crate::model::category::Category;
use crate::queue::Queue;
use crate::traits::ViewExt;

use crate::ui::listview::ListView;

pub struct BrowseView {
    list: ListView<Category>,
}

impl BrowseView {
    pub fn new(queue: Arc<Queue>, library: Arc<Library>) -> Self {
        // The categories come from the Web API, and this runs while the interface is still being
        // built: fetching them here would hold the first frame back by however long Spotify takes
        // to answer, which is the whole of startup if the endpoint is rate limiting. Start with an
        // empty list and fill it in from a worker instead.
        let categories = Arc::new(RwLock::new(Vec::new()));
        let list = ListView::new(categories.clone(), queue.clone(), library.clone());
        let pagination = list.get_pagination().clone();

        thread::spawn(move || {
            let fetched = queue.get_spotify().api.categories().into_store(categories);
            fetched.apply_pagination(&pagination);
            library.trigger_redraw();
        });

        Self { list }
    }
}

impl ViewWrapper for BrowseView {
    wrap_impl!(self.list: ListView<Category>);
}

impl ViewExt for BrowseView {
    fn title(&self) -> String {
        "Browse".to_string()
    }

    fn on_command(&mut self, s: &mut Cursive, cmd: &Command) -> Result<CommandResult, String> {
        self.list.on_command(s, cmd)
    }
}
