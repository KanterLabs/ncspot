//! The picker the `cast` command opens: every device playback can move to, and a
//! way to bring it back.

use std::sync::Arc;
use std::thread;

use cursive::Cursive;
use cursive::traits::Scrollable;
use cursive::views::{Dialog, SelectView, TextView};

use crate::cast::{self, Target};
use crate::model::playable::Playable;
use crate::queue::Queue;
use crate::spotify::{PlayerEvent, Spotify};
use crate::ui::modal::Modal;
use crate::ui::osd;

#[derive(Clone)]
enum Choice {
    Target(Target),
    /// Bring playback back to this machine.
    Home,
}

/// What is playing, where in it, and whether it is playing, to carry across.
pub fn resume_point(spotify: &Spotify, queue: &Queue) -> Option<(Playable, u32, bool)> {
    let playable = queue.get_current()?;
    let position = spotify.get_current_progress().as_millis() as u32;
    let playing = matches!(spotify.get_current_status(), PlayerEvent::Playing(_));
    Some((playable, position, playing))
}

/// Stop casting and play here again.
pub fn come_home(spotify: &Spotify, queue: &Queue) {
    let Some(name) = spotify.cast_target() else {
        osd::notify("Not casting");
        return;
    };
    spotify.stop_cast(resume_point(spotify, queue));
    osd::notify(format!("Stopped casting to {name}"));
}

/// Look for devices in the background and then offer them.
pub fn open(s: &mut Cursive, spotify: Spotify, queue: Arc<Queue>, roku_hosts: Vec<String>) {
    s.add_layer(Modal::new(
        Dialog::around(TextView::new("Looking for devices…")).title("Cast"),
    ));
    let sink = s.cb_sink().clone();
    thread::spawn(move || {
        let targets = cast::targets(&spotify.api, &roku_hosts);
        let _ = sink.send(Box::new(move |s: &mut Cursive| {
            s.pop_layer();
            s.add_layer(picker(spotify, queue, targets));
        }));
    });
}

fn picker(spotify: Spotify, queue: Arc<Queue>, targets: Vec<Target>) -> Modal<Dialog> {
    let casting = spotify.cast_target();
    let mut select = SelectView::<Choice>::new();
    if let Some(name) = &casting {
        select.add_item(
            format!("⏏  Stop casting to {name}, play here"),
            Choice::Home,
        );
    }
    for target in targets {
        let label = match &target {
            Target::Connect { name, kind, .. } if casting.as_deref() == Some(name.as_str()) => {
                format!("▸  {name}  ({kind}, playing)")
            }
            Target::Connect { name, kind, .. } => format!("   {name}  ({kind})"),
            Target::Roku(roku) if roku.has_spotify => {
                format!("   {}  (Roku, opens Spotify)", roku.name)
            }
            Target::Roku(roku) => format!("   {}  (Roku, no Spotify app)", roku.name),
        };
        select.add_item(label, Choice::Target(target));
    }

    let empty = select.is_empty();
    select.set_on_submit(move |s, choice: &Choice| {
        s.pop_layer();
        match choice.clone() {
            Choice::Home => come_home(&spotify, &queue),
            Choice::Target(target) => start(s, spotify.clone(), queue.clone(), target),
        }
    });

    let dialog = if empty {
        Dialog::around(TextView::new(
            "No devices found.\n\nOpen Spotify on the device you want, or add your \
             Roku's address to `roku_hosts` in the config if it is not found.",
        ))
    } else {
        Dialog::around(select.scrollable())
    };
    Modal::new(dialog.title("Cast to").dismiss_button("Close"))
}

/// Connect to `target` in the background, then move playback onto it.
fn start(s: &mut Cursive, spotify: Spotify, queue: Arc<Queue>, target: Target) {
    let waking = matches!(target, Target::Roku(_));
    if waking {
        s.add_layer(Modal::new(
            Dialog::around(TextView::new(format!(
                "Opening Spotify on {}…",
                target.name()
            )))
            .title("Cast"),
        ));
    }
    let sink = s.cb_sink().clone();
    thread::spawn(move || {
        let connected = cast::connect(&spotify.api, &target);
        let _ = sink.send(Box::new(move |s: &mut Cursive| {
            if waking {
                s.pop_layer();
            }
            match connected {
                Ok((id, name)) => {
                    spotify.cast_to(id, name.clone(), resume_point(&spotify, &queue));
                    osd::notify(format!("Casting to {name}"));
                }
                Err(message) => osd::notify(message),
            }
        }));
    });
}
