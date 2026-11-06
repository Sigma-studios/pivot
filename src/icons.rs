//! App icon lookup: app_id -> .desktop entry -> icon theme -> rasterized pixmap (cached).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use resvg::tiny_skia::{FilterQuality, Pixmap, PixmapPaint, Transform};
use resvg::usvg;

pub struct IconCache {
    theme: String,
    desktop: HashMap<String, String>,
    desktop_scanned: Option<Instant>,
    icons: HashMap<(String, u32), Option<Pixmap>>,
}

impl IconCache {
    pub fn new(theme: Option<String>) -> Self {
        let theme = theme.or_else(crate::config::cosmic_icon_theme).unwrap_or_else(|| "Cosmic".into());
        Self { theme, desktop: HashMap::new(), desktop_scanned: None, icons: HashMap::new() }
    }

    pub fn set_theme(&mut self, theme: Option<String>) {
        let theme = theme.or_else(crate::config::cosmic_icon_theme).unwrap_or_else(|| "Cosmic".into());
        if theme != self.theme {
            self.theme = theme;
            self.icons.clear();
        }
    }

    /// Rescan .desktop files at most every 30s (new apps get installed while we run).
    pub fn refresh_desktop_index(&mut self) {
        if self.desktop_scanned.is_some_and(|t| t.elapsed() < Duration::from_secs(30)) {
            return;
        }
        self.desktop_scanned = Some(Instant::now());
        let mut map = HashMap::new();
        for dir in data_dirs() {
            scan_desktop_dir(&dir.join("applications"), &mut map);
        }
        if map.len() != self.desktop.len() {
            self.icons.retain(|_, v| v.is_some());
        }
        self.desktop = map;
    }

    pub fn get(&mut self, app_id: &str, px: u32) -> Option<&Pixmap> {
        let key = (app_id.to_string(), px);
        if !self.icons.contains_key(&key) {
            let pm = self.load(app_id, px);
            self.icons.insert(key.clone(), pm);
        }
        self.icons.get(&key).and_then(|o| o.as_ref())
    }

    fn icon_name(&self, app_id: &str) -> String {
        let lower = app_id.to_lowercase();
        if let Some(i) = self.desktop.get(&lower) {
            return i.clone();
        }
        // Reverse-DNS desktop files: "org.foo.Bar.desktop" for app_id "bar".
        if let Some((_, i)) = self.desktop.iter().find(|(k, _)| k.rsplit('.').next() == Some(lower.as_str())) {
            return i.clone();
        }
        app_id.to_string()
    }

    fn load(&self, app_id: &str, px: u32) -> Option<Pixmap> {
        if app_id.is_empty() {
            return None;
        }
        let name = self.icon_name(app_id);
        let path = if Path::new(&name).is_absolute() {
            Some(PathBuf::from(&name))
        } else {
            let lookup = |n: &str| {
                freedesktop_icons::lookup(n).with_size(px.min(512) as u16).with_theme(&self.theme).with_cache().find()
            };
            lookup(&name).or_else(|| lookup(&name.to_lowercase()))
        }?;
        rasterize(&path, px)
    }
}

fn data_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    match std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).filter(|p| p.is_absolute()) {
        Some(p) => v.push(p),
        None => v.push(crate::config::home().join(".local/share")),
    }
    let sys = std::env::var("XDG_DATA_DIRS").unwrap_or_default();
    let sys = if sys.is_empty() { "/usr/local/share:/usr/share".to_string() } else { sys };
    v.extend(sys.split(':').filter(|s| !s.is_empty()).map(PathBuf::from));
    for extra in ["/var/lib/flatpak/exports/share", "/var/lib/snapd/desktop"] {
        let p = PathBuf::from(extra);
        if !v.contains(&p) {
            v.push(p);
        }
    }
    let user_flatpak = crate::config::home().join(".local/share/flatpak/exports/share");
    if !v.contains(&user_flatpak) {
        v.push(user_flatpak);
    }
    v
}

fn scan_desktop_dir(dir: &Path, map: &mut HashMap<String, String>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let path = e.path();
        if path.is_dir() {
            scan_desktop_dir(&path, map);
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("desktop") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let mut icon = None;
        let mut wm_class = None;
        let mut in_entry = false;
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                in_entry = line == "[Desktop Entry]";
                continue;
            }
            if !in_entry {
                continue;
            }
            if let Some(v) = line.strip_prefix("Icon=") {
                icon = Some(v.trim().to_string());
            } else if let Some(v) = line.strip_prefix("StartupWMClass=") {
                wm_class = Some(v.trim().to_lowercase());
            }
        }
        let Some(icon) = icon.filter(|i| !i.is_empty()) else { continue };
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            // Earlier data dirs take precedence (user overrides system).
            map.entry(stem.to_lowercase()).or_insert_with(|| icon.clone());
        }
        if let Some(wm) = wm_class {
            map.entry(wm).or_insert(icon);
        }
    }
}

fn rasterize(path: &Path, px: u32) -> Option<Pixmap> {
    let data = std::fs::read(path).ok()?;
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase();
    let mut out = Pixmap::new(px, px)?;
    if ext == "svg" || ext == "svgz" {
        let tree = usvg::Tree::from_data(&data, &usvg::Options::default()).ok()?;
        let size = tree.size();
        let s = px as f32 / size.width().max(size.height());
        let dx = (px as f32 - size.width() * s) / 2.0;
        let dy = (px as f32 - size.height() * s) / 2.0;
        resvg::render(&tree, Transform::from_scale(s, s).post_translate(dx, dy), &mut out.as_mut());
    } else {
        let src = Pixmap::decode_png(&data).ok()?;
        let s = px as f32 / src.width().max(src.height()) as f32;
        let dx = (px as f32 - src.width() as f32 * s) / 2.0;
        let dy = (px as f32 - src.height() as f32 * s) / 2.0;
        let paint = PixmapPaint { quality: FilterQuality::Bicubic, ..Default::default() };
        out.draw_pixmap(0, 0, src.as_ref(), &paint, Transform::from_scale(s, s).post_translate(dx, dy), None);
    }
    Some(out)
}
