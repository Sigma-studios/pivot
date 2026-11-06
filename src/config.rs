//! User configuration (`$XDG_CONFIG_HOME/pivot/config.toml`) and color palette resolution.

use serde::Deserialize;
use std::path::PathBuf;

#[derive(Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    #[default]
    Auto,
    Dark,
    Light,
}

/// How the selected window is highlighted.
#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SelectionStyle {
    /// Accent outline + soft accent fill.
    #[default]
    Both,
    Outline,
    Fill,
    /// Accent bar under the selected window.
    Underline,
    /// Outline with a soft outer glow.
    Glow,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Config {
    /// Upper bound on preview refresh rate. 0 = no cap: every preview updates as fast as its
    /// window redraws, which is bounded by the monitor refresh rate (60/144/... Hz).
    pub max_fps: u32,
    /// How long Alt must stay held before the switcher becomes visible. A quick Alt+Tab tap
    /// switches to the previous window without flashing the UI (like Windows).
    pub show_delay_ms: u64,
    /// Preferred thumbnail height in logical pixels. Shrinks automatically to fit many windows.
    pub thumbnail_height: f32,
    pub min_thumbnail_height: f32,
    /// Maximum fraction of the output the panel may cover.
    pub max_width_fraction: f32,
    pub max_height_fraction: f32,
    pub font_size: f32,
    pub icon_size: f32,
    pub corner_radius: f32,
    /// Panel background opacity (0 = no panel at all, windows float over the backdrop).
    pub panel_opacity: f32,
    /// Panel outline width in logical px (0 = none).
    pub border_width: f32,
    pub border_color: Option<String>,
    /// Opacity of a rounded card drawn behind every window (0 = none). Useful with a
    /// transparent or absent panel so titles stay readable.
    pub card_opacity: f32,
    pub selection: SelectionStyle,
    /// Contrast aids for transparent styles, all 0 = off (0..1 opacity):
    /// dark halo around title text,
    pub text_shadow: f32,
    /// soft drop shadow under each preview,
    pub thumb_shadow: f32,
    /// dark pill behind each icon + title,
    pub title_pill: f32,
    /// and a feathered dark scrim behind the whole grid (fades out over `scrim_feather` px).
    pub scrim: f32,
    pub scrim_feather: f32,
    /// Outline width of the selection highlight in logical px.
    pub selection_width: f32,
    pub theme: ThemeMode,
    /// Hex colors (`#rrggbb` or `#rrggbbaa`). Unset = derived from the COSMIC theme.
    pub accent: Option<String>,
    pub panel_color: Option<String>,
    pub text_color: Option<String>,
    /// Color drawn over the whole screen behind the panel.
    pub backdrop: String,
    /// GPU buffers per preview (2 or 3). 3 gives smoother high refresh rates at more VRAM.
    pub capture_buffers: usize,
    pub include_minimized: bool,
    pub paint_cursors: bool,
    /// Skip dmabuf and capture through shared memory (slow; for debugging driver issues).
    pub force_shm: bool,
    pub icon_theme: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_fps: 0,
            show_delay_ms: 70,
            thumbnail_height: 190.0,
            min_thumbnail_height: 70.0,
            max_width_fraction: 0.92,
            max_height_fraction: 0.86,
            font_size: 13.0,
            icon_size: 20.0,
            corner_radius: 14.0,
            panel_opacity: 0.0,
            border_width: 0.0,
            border_color: None,
            card_opacity: 0.0,
            selection: SelectionStyle::Glow,
            text_shadow: 0.0,
            thumb_shadow: 0.0,
            title_pill: 0.0,
            scrim: 0.0,
            scrim_feather: 72.0,
            selection_width: 2.0,
            theme: ThemeMode::Auto,
            accent: None,
            panel_color: None,
            text_color: None,
            backdrop: "#000000a8".into(),
            capture_buffers: 2,
            include_minimized: true,
            paint_cursors: false,
            force_shm: false,
            icon_theme: None,
        }
    }
}

pub fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home().join(".config"))
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| "/".into())
}

impl Config {
    pub fn path() -> PathBuf {
        config_dir().join("pivot").join("config.toml")
    }

    pub fn load() -> Self {
        let path = Self::path();
        let mut cfg = match std::fs::read_to_string(&path) {
            Ok(s) => match toml::from_str::<Config>(&s) {
                Ok(c) => c,
                Err(e) => {
                    log::warn!("invalid {}: {e}; using defaults", path.display());
                    Config::default()
                }
            },
            Err(_) => Config::default(),
        };
        cfg.capture_buffers = cfg.capture_buffers.clamp(2, 3);
        cfg.thumbnail_height = cfg.thumbnail_height.clamp(40.0, 1000.0);
        cfg.min_thumbnail_height = cfg.min_thumbnail_height.clamp(30.0, cfg.thumbnail_height);
        cfg.max_width_fraction = cfg.max_width_fraction.clamp(0.2, 1.0);
        cfg.max_height_fraction = cfg.max_height_fraction.clamp(0.2, 1.0);
        cfg.font_size = cfg.font_size.clamp(6.0, 48.0);
        cfg.icon_size = cfg.icon_size.clamp(8.0, 96.0);
        cfg.panel_opacity = cfg.panel_opacity.clamp(0.0, 1.0);
        cfg.card_opacity = cfg.card_opacity.clamp(0.0, 1.0);
        cfg.border_width = cfg.border_width.clamp(0.0, 8.0);
        cfg.selection_width = cfg.selection_width.clamp(0.0, 8.0);
        for v in [&mut cfg.text_shadow, &mut cfg.thumb_shadow, &mut cfg.title_pill, &mut cfg.scrim] {
            *v = v.clamp(0.0, 1.0);
        }
        cfg.scrim_feather = cfg.scrim_feather.clamp(8.0, 240.0).round();
        cfg
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Rgba {
    pub const fn new(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }

    pub fn with_alpha(self, a: f32) -> Self {
        Self { a, ..self }
    }

    pub fn parse(s: &str) -> Option<Self> {
        let h = s.trim().trim_start_matches('#');
        let byte = |i: usize| u8::from_str_radix(h.get(i..i + 2)?, 16).ok().map(|v| v as f32 / 255.0);
        match h.len() {
            6 => Some(Self::new(byte(0)?, byte(2)?, byte(4)?, 1.0)),
            8 => Some(Self::new(byte(0)?, byte(2)?, byte(4)?, byte(6)?)),
            _ => None,
        }
    }

    pub fn to_skia(self) -> tiny_skia::Color {
        tiny_skia::Color::from_rgba(
            self.r.clamp(0.0, 1.0),
            self.g.clamp(0.0, 1.0),
            self.b.clamp(0.0, 1.0),
            self.a.clamp(0.0, 1.0),
        )
        .unwrap_or(tiny_skia::Color::BLACK)
    }

    /// Premultiplied ARGB8888 pixel value (wl_shm native format).
    pub fn to_argb_premul(self) -> u32 {
        let a = self.a.clamp(0.0, 1.0);
        let c = |v: f32| ((v.clamp(0.0, 1.0) * a) * 255.0).round() as u32;
        ((a * 255.0).round() as u32) << 24 | c(self.r) << 16 | c(self.g) << 8 | c(self.b)
    }
}

#[derive(Clone, Debug)]
pub struct Palette {
    pub panel: Rgba,
    pub panel_border: Rgba,
    pub card: Rgba,
    pub text: Rgba,
    pub accent: Rgba,
    pub selected: Rgba,
    pub placeholder: Rgba,
    pub backdrop: Rgba,
    pub close_bg: Rgba,
}

impl Palette {
    pub fn resolve(cfg: &Config) -> Self {
        let dark = match cfg.theme {
            ThemeMode::Dark => true,
            ThemeMode::Light => false,
            ThemeMode::Auto => cosmic_is_dark().unwrap_or(true),
        };
        let accent = cfg
            .accent
            .as_deref()
            .and_then(Rgba::parse)
            .or_else(|| cosmic_accent(dark))
            .unwrap_or(Rgba::new(0.39, 0.82, 0.87, 1.0));
        let (panel, text, fg) = if dark {
            (Rgba::new(0.106, 0.106, 0.114, 0.975), Rgba::new(0.93, 0.93, 0.95, 1.0), 1.0)
        } else {
            (Rgba::new(0.95, 0.95, 0.96, 0.975), Rgba::new(0.08, 0.08, 0.1, 1.0), 0.0)
        };
        let panel = cfg.panel_color.as_deref().and_then(Rgba::parse).unwrap_or(panel).with_alpha(cfg.panel_opacity);
        let text = cfg.text_color.as_deref().and_then(Rgba::parse).unwrap_or(text);
        Self {
            panel,
            panel_border: cfg.border_color.as_deref().and_then(Rgba::parse).unwrap_or(Rgba::new(fg, fg, fg, 0.10)),
            card: panel.with_alpha(cfg.card_opacity),
            text,
            accent,
            selected: accent.with_alpha(0.22),
            placeholder: Rgba::new(fg, fg, fg, 0.06),
            backdrop: Rgba::parse(&cfg.backdrop).unwrap_or(Rgba::new(0.0, 0.0, 0.0, 0.0)),
            close_bg: Rgba::new(0.85, 0.22, 0.22, 0.95),
        }
    }
}

fn cosmic_config(component: &str, key: &str) -> Option<String> {
    std::fs::read_to_string(config_dir().join("cosmic").join(component).join("v1").join(key)).ok()
}

fn cosmic_is_dark() -> Option<bool> {
    cosmic_config("com.system76.CosmicTheme.Mode", "is_dark").and_then(|s| s.trim().parse().ok())
}

/// Pull the accent color out of COSMIC's RON theme file without a full RON parser:
/// the first `red:`/`green:`/`blue:` triple in `accent` is the accent's base color.
fn cosmic_accent(dark: bool) -> Option<Rgba> {
    let comp = if dark { "com.system76.CosmicTheme.Dark" } else { "com.system76.CosmicTheme.Light" };
    let s = cosmic_config(comp, "accent")?;
    let num = |key: &str| -> Option<f32> {
        let i = s.find(key)? + key.len();
        let rest = s[i..].trim_start();
        let end = rest.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == 'e'))?;
        rest[..end].parse().ok()
    };
    Some(Rgba::new(num("red:")?, num("green:")?, num("blue:")?, 1.0))
}

/// Icon theme configured in COSMIC (`com.system76.CosmicTk/v1/icon_theme` holds a RON string).
pub fn cosmic_icon_theme() -> Option<String> {
    let s = cosmic_config("com.system76.CosmicTk", "icon_theme")?;
    let t = s.trim().trim_matches('"').to_string();
    (!t.is_empty()).then_some(t)
}
