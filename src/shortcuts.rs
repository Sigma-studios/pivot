//! `pivot setup-shortcuts`: bind Alt/Super(+Shift)+Tab to pivot in COSMIC's custom shortcuts.
//! Custom shortcuts override COSMIC's built-in WindowSwitcher bindings for the same keys.

use std::io;
use std::path::PathBuf;

pub fn path() -> PathBuf {
    crate::config::config_dir().join("cosmic/com.system76.CosmicSettings.Shortcuts/v1/custom")
}

fn entries(cmd: &str) -> String {
    let mut s = String::new();
    for (mods, action) in [("Alt", "next"), ("Alt, Shift", "prev"), ("Super", "next"), ("Super, Shift", "prev")] {
        s.push_str(&format!("    (modifiers: [{mods}], key: \"Tab\"): Spawn(\"{cmd} {action}\"),\n"));
    }
    s
}

/// Insert our bindings into an existing RON map, or create one. Returns None if already present.
pub fn merge(existing: Option<&str>, cmd: &str) -> Result<Option<String>, String> {
    let new = entries(cmd);
    let Some(text) = existing.map(str::trim).filter(|t| !t.is_empty()) else {
        return Ok(Some(format!("{{\n{new}}}\n")));
    };
    if text.contains(&format!("\"{cmd} next\"")) {
        return Ok(None);
    }
    let body = text.strip_suffix('}').ok_or("unexpected format: file does not end with '}'")?.trim_end();
    let sep = if body.ends_with('{') || body.ends_with(',') { "" } else { "," };
    Ok(Some(format!("{body}{sep}\n{new}}}\n")))
}

/// Remove every binding that spawns `cmd next|prev`. Returns None if there was nothing to remove.
pub fn strip(existing: &str, cmd: &str) -> Option<String> {
    let ours = |l: &str| l.contains(&format!("Spawn(\"{cmd} next\")")) || l.contains(&format!("Spawn(\"{cmd} prev\")"));
    if !existing.lines().any(ours) {
        return None;
    }
    let mut out: String = existing.lines().filter(|l| !ours(l)).map(|l| format!("{l}\n")).collect();
    // Tidy a trailing comma left before the closing brace.
    if let Some(i) = out.rfind('}') {
        let head = out[..i].trim_end();
        if let Some(h) = head.strip_suffix(',') {
            out = format!("{h}\n}}\n");
        }
    }
    Some(out)
}

pub fn remove(cmd: &str) -> io::Result<()> {
    let path = path();
    let Ok(existing) = std::fs::read_to_string(&path) else {
        println!("no user shortcut file at {}", path.display());
        return Ok(());
    };
    match strip(&existing, cmd) {
        None => println!("no `{cmd}` shortcuts in {}", path.display()),
        Some(text) => {
            std::fs::write(path.with_extension("bak-pivot"), &existing)?;
            std::fs::write(&path, text)?;
            println!("removed `{cmd}` shortcuts from {} (COSMIC's own switcher is back)", path.display());
        }
    }
    Ok(())
}

/// Daemon startup hook (`pivot daemon --ensure-shortcuts`). Only touches the user file if it
/// already exists: when it doesn't, COSMIC falls back to the system-wide `custom` file that the
/// NixOS module installs, and there is nothing to do.
pub fn ensure_in_user_file(cmd: &str) {
    let path = path();
    let Ok(existing) = std::fs::read_to_string(&path) else { return };
    match merge(Some(&existing), cmd) {
        Ok(None) => {}
        Ok(Some(text)) => {
            let res = std::fs::write(path.with_extension("bak-pivot"), &existing).and_then(|_| std::fs::write(&path, text));
            match res {
                Ok(()) => log::info!("added pivot Alt+Tab bindings to {}", path.display()),
                Err(e) => log::warn!("could not update {}: {e}", path.display()),
            }
        }
        Err(e) => log::warn!("not touching {}: {e}", path.display()),
    }
}

/// `cmd` is what COSMIC will run; "pivot" (resolved via PATH) survives Nix store path changes.
pub fn install(cmd: &str) -> io::Result<()> {
    let path = path();
    let existing = std::fs::read_to_string(&path).ok();
    match merge(existing.as_deref(), cmd).map_err(io::Error::other)? {
        None => println!("pivot shortcuts already present in {}", path.display()),
        Some(text) => {
            if let Some(old) = &existing {
                let backup = path.with_extension("bak-pivot");
                std::fs::write(&backup, old)?;
                println!("backup: {}", backup.display());
            }
            std::fs::create_dir_all(path.parent().unwrap())?;
            std::fs::write(&path, text)?;
            println!("bound Alt+Tab / Super+Tab (and Shift variants) to `{cmd}` in {}", path.display());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges() {
        let fresh = merge(None, "pivot").unwrap().unwrap();
        assert!(fresh.starts_with("{\n") && fresh.trim_end().ends_with('}'));
        assert!(merge(Some(&fresh), "pivot").unwrap().is_none());

        let user = "{\n    (modifiers: [Super], key: \"t\"): Spawn(\"cosmic-term\")\n}\n";
        let merged = merge(Some(user), "pivot").unwrap().unwrap();
        assert!(merged.contains("Spawn(\"cosmic-term\"),\n    (modifiers: [Alt], key: \"Tab\"): Spawn(\"pivot next\")"));
        assert!(merge(Some("{}"), "pivot").unwrap().unwrap().contains("pivot prev"));
        assert!(merge(Some("garbage"), "pivot").is_err());

        let stripped = strip(&merged, "pivot").unwrap();
        assert!(!stripped.contains("pivot"));
        assert!(stripped.contains("Spawn(\"cosmic-term\")\n}"), "{stripped}");
        assert!(strip(user, "pivot").is_none());
        assert_eq!(strip(&fresh, "pivot").unwrap().trim(), "{\n}".trim());
    }
}
