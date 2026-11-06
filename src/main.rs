//! pivot — a live-preview Alt+Tab switcher for the COSMIC desktop.

mod app;
mod buffers;
mod capture;
mod config;
mod icons;
mod ipc;
mod layout;
mod render;
mod shortcuts;

use ipc::Command;

const USAGE: &str = "\
pivot — live-preview window switcher for COSMIC

USAGE:
    pivot daemon [--ensure-shortcuts]
                        run the background daemon (autostarted by the commands below);
                        --ensure-shortcuts adds the Alt+Tab bindings to an existing user
                        shortcut file if they are missing
    pivot next          open switcher / select next window   (bind to Alt+Tab)
    pivot prev          open switcher / select previous      (bind to Alt+Shift+Tab)
    pivot show          open switcher that stays open until Enter/click/Esc
    pivot toggle        show or hide the sticky switcher
    pivot hide          close the switcher
    pivot quit          stop the daemon
    pivot setup-shortcuts [CMD]
                        bind Alt/Super(+Shift)+Tab in COSMIC to `CMD next|prev` (default: pivot)
    pivot remove-shortcuts [CMD]
                        remove those bindings again (COSMIC's own switcher comes back)

While open: Tab/Shift+Tab, arrows or hjkl move; release Alt, Enter or click to switch;
Esc cancels; W/Q/Delete or middle-click closes the selected window.

Config: ~/.config/pivot/config.toml   Logs: RUST_LOG=pivot=debug pivot daemon";

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("pivot=info")).init();
    let arg = std::env::args().nth(1).unwrap_or_default();
    let code = match arg.as_str() {
        "daemon" => match ipc::bind() {
            Ok(listener) => {
                if std::env::args().any(|a| a == "--ensure-shortcuts") {
                    shortcuts::ensure_in_user_file("pivot");
                }
                match app::run(listener) {
                Ok(()) => 0,
                    Err(e) => {
                        log::error!("{e}");
                        1
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                log::info!("{e}");
                0
            }
            Err(e) => {
                log::error!("cannot bind {}: {e}", ipc::socket_path().display());
                1
            }
        },
        "setup-shortcuts" => {
            let cmd = std::env::args().nth(2).unwrap_or_else(|| "pivot".into());
            match shortcuts::install(&cmd) {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("pivot: {e}");
                    1
                }
            }
        }
        "remove-shortcuts" => {
            let cmd = std::env::args().nth(2).unwrap_or_else(|| "pivot".into());
            match shortcuts::remove(&cmd) {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("pivot: {e}");
                    1
                }
            }
        }
        "-h" | "--help" | "help" | "" => {
            println!("{USAGE}");
            0
        }
        other => match Command::parse(other) {
            Some(cmd) => match ipc::send(cmd, true) {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("pivot: cannot reach daemon: {e}");
                    1
                }
            },
            None => {
                eprintln!("pivot: unknown command {other:?}\n\n{USAGE}");
                2
            }
        },
    };
    std::process::exit(code);
}
