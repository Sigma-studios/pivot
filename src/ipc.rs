//! Tiny line-based control socket. `pivot next` (bound to Alt+Tab) is a short-lived client that
//! writes one word to the daemon and exits; it auto-starts the daemon if needed.

use std::io::{self, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::{Command as Proc, Stdio};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// Open (Alt-release mode) or move selection forward.
    Next,
    /// Open (Alt-release mode) or move selection backward.
    Prev,
    /// Open in sticky mode: stays open until Enter/click/Escape.
    Show,
    Toggle,
    Hide,
    Quit,
}

impl Command {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "next" => Self::Next,
            "prev" => Self::Prev,
            "show" => Self::Show,
            "toggle" => Self::Toggle,
            "hide" => Self::Hide,
            "quit" => Self::Quit,
            _ => return None,
        })
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Next => "next",
            Self::Prev => "prev",
            Self::Show => "show",
            Self::Toggle => "toggle",
            Self::Hide => "hide",
            Self::Quit => "quit",
        }
    }
}

pub fn socket_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-0".into());
    let display = display.rsplit('/').next().unwrap_or("wayland-0").to_string();
    dir.join(format!("pivot-{display}.sock"))
}

/// Bind the daemon socket, refusing if another daemon is alive.
pub fn bind() -> io::Result<UnixListener> {
    let path = socket_path();
    if UnixStream::connect(&path).is_ok() {
        return Err(io::Error::new(io::ErrorKind::AddrInUse, "pivot daemon already running"));
    }
    let _ = std::fs::remove_file(&path);
    UnixListener::bind(&path)
}

pub fn send(cmd: Command, autostart: bool) -> io::Result<()> {
    let path = socket_path();
    let mut stream = match UnixStream::connect(&path) {
        Ok(s) => s,
        Err(e) if autostart && cmd != Command::Quit && cmd != Command::Hide => {
            log::info!("daemon not running ({e}); starting it");
            Proc::new(std::env::current_exe()?)
                .arg("daemon")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()?;
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                match UnixStream::connect(&path) {
                    Ok(s) => break s,
                    Err(e) if Instant::now() > deadline => return Err(e),
                    Err(_) => std::thread::sleep(Duration::from_millis(10)),
                }
            }
        }
        Err(e) => return Err(e),
    };
    stream.write_all(format!("{}\n", cmd.as_str()).as_bytes())
}
