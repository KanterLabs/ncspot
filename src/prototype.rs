//! Launch the optional OpenTUI frontend without handing it the playback session.
#[cfg(unix)]
use std::path::{Path, PathBuf};

#[cfg(unix)]
fn executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(unix)]
fn find_on_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|directory| directory.join(name))
            .find(|path| executable(path))
    })
}

#[cfg(unix)]
fn frontend_binary() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("RESONANCE_OPENTUI_BIN") {
        let path = PathBuf::from(path);
        if path.is_absolute() && executable(&path) {
            return Ok(path);
        }
        return Err("RESONANCE_OPENTUI_BIN must point to an executable absolute path".into());
    }
    let sibling = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|parent| parent.join("resonance-opentui")));
    sibling
        .filter(|path| executable(path))
        .or_else(|| find_on_path("resonance-opentui"))
        .ok_or_else(|| {
            "OpenTUI prototype is missing: install resonance-opentui beside Resonance".into()
        })
}

/// Separate argv entries preserve paths verbatim, including spaces and shell characters.
#[cfg(unix)]
fn terminal_args(terminal: &str, binary: &Path, socket: &Path) -> Vec<std::ffi::OsString> {
    let prefix: &[&str] = match terminal {
        "xdg-terminal-exec" | "gnome-terminal" => &["--"],
        "wezterm" => &["start", "--"],
        "kitty" => &[],
        _ => &["-e"],
    };
    let mut args: Vec<_> = prefix.iter().map(std::ffi::OsString::from).collect();
    args.extend([
        binary.as_os_str().to_owned(),
        "--socket".into(),
        socket.as_os_str().to_owned(),
    ]);
    args
}

#[cfg(unix)]
pub fn open(socket: &Path, events: crate::events::EventManager) -> Result<(), String> {
    use std::process::{Command, Stdio};
    let binary = frontend_binary()?;
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        return Err(
            "F5 needs a desktop terminal; run resonance-opentui --socket PATH in another terminal"
                .into(),
        );
    }
    for terminal in [
        "xdg-terminal-exec",
        "gnome-terminal",
        "konsole",
        "kitty",
        "alacritty",
        "wezterm",
        "xterm",
    ] {
        let Some(program) = find_on_path(terminal) else {
            continue;
        };
        match Command::new(program)
            .args(terminal_args(terminal, &binary, socket))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(mut child) => {
                std::thread::spawn(move || {
                    if let Ok(status) = child.wait()
                        && !status.success()
                    {
                        log::error!("OpenTUI terminal exited with {status}");
                        crate::ui::osd::notify(
                            "OpenTUI terminal failed to open; run resonance-opentui --socket PATH manually",
                        );
                        events.try_trigger();
                    }
                });
                return Ok(());
            }
            Err(error) => log::warn!("Could not open {terminal}: {error}"),
        }
    }
    Err("No supported desktop terminal found for the OpenTUI prototype".into())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn launcher_preserves_socket_and_binary_paths_without_shell_interpolation() {
        let binary = Path::new("/tmp/music player/$(echo nope)");
        let socket = Path::new("/tmp/radio session;literal.sock");
        let args = terminal_args("gnome-terminal", binary, socket);
        assert_eq!(
            args,
            vec![
                "--",
                binary.to_str().unwrap(),
                "--socket",
                socket.to_str().unwrap()
            ]
        );
        let args = terminal_args("xterm", binary, socket);
        assert_eq!(args[0], "-e");
        assert_eq!(args[3], socket.as_os_str());
    }
}
