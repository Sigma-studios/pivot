//! CPU drawing of the switcher chrome (panel, highlights, titles, icons). This is only redrawn when
//! the selection/titles change; the live previews themselves are separate GPU buffers composited by
//! COSMIC directly, so this never runs per video frame.

use std::collections::HashMap;

use cosmic_text::{Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, SwashCache, Wrap};
use resvg::tiny_skia::{
    FillRule, LineCap, Paint, Path, PathBuilder, Pixmap, PixmapPaint, Stroke, Transform,
};

use crate::config::{Config, Palette, Rgba};
use crate::icons::IconCache;
use crate::layout::{Layout, Rect};

pub struct Item<'a> {
    pub title: &'a str,
    pub app_id: &'a str,
    pub has_preview: bool,
}

pub struct Renderer {
    fonts: FontSystem,
    swash: SwashCache,
    text_cache: HashMap<(String, u32, u32, u32), Option<Pixmap>>,
    shadow_cache: HashMap<[u32; 5], Pixmap>,
    pub icons: IconCache,
}

impl Renderer {
    pub fn new(icon_theme: Option<String>) -> Self {
        Self {
            fonts: FontSystem::new(),
            swash: SwashCache::new(),
            text_cache: HashMap::new(),
            shadow_cache: HashMap::new(),
            icons: IconCache::new(icon_theme),
        }
    }

    /// Drop per-session caches so the idle daemon stays small.
    pub fn trim(&mut self) {
        self.text_cache.clear();
        self.shadow_cache.clear();
        self.swash = SwashCache::new();
    }

    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        cfg: &Config,
        pal: &Palette,
        layout: &Layout,
        items: &[Item],
        selected: usize,
        close_hover: bool,
        scale: f32,
    ) -> Option<Pixmap> {
        let pw = (layout.panel.w * scale).ceil() as u32;
        let ph = (layout.panel.h * scale).ceil() as u32;
        let mut pm = Pixmap::new(pw.max(1), ph.max(1))?;
        let t = Transform::from_scale(scale, scale);
        let full = Rect { x: 0.0, y: 0.0, w: layout.panel.w, h: layout.panel.h };

        if pal.panel.a > 0.0 {
            fill_rrect(&mut pm, full, cfg.corner_radius, pal.panel, t);
        }
        if cfg.border_width > 0.0 {
            let bw = cfg.border_width;
            stroke_rrect(&mut pm, inset(full, bw / 2.0), cfg.corner_radius - bw / 2.0, pal.panel_border, bw, t);
        }

        let cell_r = (cfg.corner_radius * 0.65).max(4.0);
        let icon_px = (cfg.icon_size * scale).round() as u32;
        let font_px = cfg.font_size * scale;

        for (i, (cell, item)) in layout.cells.iter().zip(items).enumerate() {
            let sel = i == selected;
            if pal.card.a > 0.0 {
                fill_rrect(&mut pm, cell.rect, cell_r, pal.card, t);
            }
            if sel {
                draw_selection(&mut pm, cfg, pal, cell, cell_r, t);
            }
            if cfg.thumb_shadow > 0.0 {
                let th = scaled(cell.thumb, scale);
                let spread = (12.0 * scale).round();
                let sh = self.shadow(th.w.round() as u32, th.h.round() as u32, 4.0 * scale, spread, cfg.thumb_shadow);
                let (x, y) = (th.x.round() - spread, th.y.round() - spread + (3.0 * scale).round());
                pm.draw_pixmap(x as i32, y as i32, sh.as_ref(), &PixmapPaint::default(), Transform::identity(), None);
            }

            // Placeholder under the preview: visible until the first frame lands, and permanently
            // for windows the compositor refuses to capture.
            if !item.has_preview {
                fill_rrect(&mut pm, cell.thumb, 6.0, pal.placeholder, t);
                let big = (cell.thumb.h.min(cell.thumb.w) * 0.42).min(96.0);
                let bx = cell.thumb.x + (cell.thumb.w - big) / 2.0;
                let by = cell.thumb.y + (cell.thumb.h - big) / 2.0;
                self.draw_icon(&mut pm, item, pal, bx * scale, by * scale, (big * scale).round() as u32);
            }

            // Header: icon + title (+ close button on the selected cell).
            let hx = cell.header.x * scale;
            let hy = cell.header.y * scale;
            let hh = cell.header.h * scale;
            let tx = hx + icon_px as f32 + 7.0 * scale;
            let mut max_w = cell.header.x * scale + cell.header.w * scale - tx;
            if sel {
                max_w -= (cell.close.w + 4.0) * scale;
            }
            let title = if item.title.is_empty() { item.app_id } else { item.title };
            let text = self.text(title, font_px, max_w, pal.text);
            if cfg.title_pill > 0.0 {
                let tw = text.as_ref().map_or(0.0, |t| t.width() as f32);
                let pad = 5.0 * scale;
                let ph = (hh - 2.0 * scale).max(icon_px as f32 + 4.0 * scale);
                let pill = Rect { x: hx - pad, y: hy + (hh - ph) / 2.0, w: tx - hx + tw + 2.0 * pad, h: ph };
                fill_rrect(&mut pm, pill, ph / 2.0, pal.panel.with_alpha(cfg.title_pill), Transform::identity());
            }
            self.draw_icon(&mut pm, item, pal, hx, hy + (hh - icon_px as f32) / 2.0, icon_px);
            if sel {
                let bg = if close_hover { pal.close_bg } else { pal.panel_border.with_alpha(0.14) };
                fill_rrect(&mut pm, cell.close, cell.close.w / 2.0, bg, t);
                let c = cell.close;
                let k = c.w * 0.3;
                let fg = if close_hover { Rgba::new(1.0, 1.0, 1.0, 1.0) } else { pal.text.with_alpha(0.85) };
                let mut pb = PathBuilder::new();
                pb.move_to(c.x + k, c.y + k);
                pb.line_to(c.x + c.w - k, c.y + c.h - k);
                pb.move_to(c.x + c.w - k, c.y + k);
                pb.line_to(c.x + k, c.y + c.h - k);
                if let Some(p) = pb.finish() {
                    stroke_path(&mut pm, &p, fg, 1.6, t);
                }
            }
            if let Some(text) = text {
                let ty = hy + (hh - text.height() as f32) / 2.0;
                if cfg.text_shadow > 0.0 {
                    let r = (2.5 * scale).ceil() as u32;
                    if let Some(h) = halo(&text, r, cfg.text_shadow) {
                        let (x, y) = (tx.round() as i32 - r as i32, ty.round() as i32 - r as i32 + scale.round() as i32);
                        pm.draw_pixmap(x, y, h.as_ref(), &PixmapPaint::default(), Transform::identity(), None);
                    }
                }
                pm.draw_pixmap(tx.round() as i32, ty.round() as i32, text.as_ref(), &PixmapPaint::default(), Transform::identity(), None);
            }
        }
        let b = bleed(cfg);
        if b > 0.0 {
            // Feathered scrim: grows the surface by `b` on every side and fades to nothing there.
            let bp = (b * scale).round();
            let mut out = self.shadow(pm.width(), pm.height(), cfg.corner_radius * scale, bp, cfg.scrim).clone();
            out.draw_pixmap(bp as i32, bp as i32, pm.as_ref(), &PixmapPaint::default(), Transform::identity(), None);
            return Some(out);
        }
        Some(pm)
    }

    /// Soft black rounded-rect shadow of a `w`×`h` rect, padded by `spread` px (cached per size).
    fn shadow(&mut self, w: u32, h: u32, rad: f32, spread: f32, opacity: f32) -> &Pixmap {
        let key = [w, h, rad.to_bits(), spread.to_bits(), opacity.to_bits()];
        self.shadow_cache.entry(key).or_insert_with(|| {
            let sp = spread as u32;
            let mut pm = Pixmap::new(w + 2 * sp, h + 2 * sp).expect("shadow size");
            let r = Rect { x: spread, y: spread, w: w as f32, h: h as f32 };
            darken_rrect(&mut pm, r, rad, spread, opacity, 0.0);
            pm
        })
    }

    fn draw_icon(&mut self, pm: &mut Pixmap, item: &Item, pal: &Palette, x: f32, y: f32, px: u32) {
        if px == 0 {
            return;
        }
        if let Some(icon) = self.icons.get(item.app_id, px) {
            pm.draw_pixmap(x.round() as i32, y.round() as i32, icon.as_ref(), &PixmapPaint::default(), Transform::identity(), None);
            return;
        }
        // Letter avatar fallback.
        let r = Rect { x, y, w: px as f32, h: px as f32 };
        fill_rrect(pm, r, px as f32 * 0.25, pal.accent.with_alpha(0.85), Transform::identity());
        let src = if item.app_id.is_empty() { item.title } else { item.app_id.rsplit('.').next().unwrap_or(item.app_id) };
        let letter: String = src.chars().find(|c| c.is_alphanumeric()).map(|c| c.to_uppercase().collect()).unwrap_or_else(|| "?".into());
        if let Some(text) = self.text(&letter, px as f32 * 0.62, px as f32, Rgba::new(1.0, 1.0, 1.0, 1.0)) {
            let tx = x + (px as f32 - text.width() as f32) / 2.0;
            let ty = y + (px as f32 - text.height() as f32) / 2.0;
            pm.draw_pixmap(tx.round() as i32, ty.round() as i32, text.as_ref(), &PixmapPaint::default(), Transform::identity(), None);
        }
    }

    /// Shaped, ellipsized, rasterized single-line text. Cached per (text, size, width, color).
    fn text(&mut self, text: &str, size_px: f32, max_w: f32, color: Rgba) -> Option<Pixmap> {
        if max_w < 4.0 || text.is_empty() {
            return None;
        }
        let key = (text.to_string(), size_px.to_bits(), max_w.to_bits(), color.to_argb_premul());
        if let Some(c) = self.text_cache.get(&key) {
            return c.clone();
        }
        let pm = self.rasterize_text(text, size_px, max_w, color);
        self.text_cache.insert(key, pm.clone());
        pm
    }

    fn rasterize_text(&mut self, text: &str, size_px: f32, max_w: f32, color: Rgba) -> Option<Pixmap> {
        let line_h = (size_px * 1.3).ceil();
        let fs = &mut self.fonts;
        let mut buf = Buffer::new(fs, Metrics::new(size_px, line_h));
        buf.set_wrap(fs, Wrap::None);
        buf.set_size(fs, None, None);
        let attrs = Attrs::new().family(Family::SansSerif);
        let single_line: String = text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
        buf.set_text(fs, &single_line, &attrs, Shaping::Advanced);
        buf.shape_until_scroll(fs, false);

        let width = |b: &Buffer| b.layout_runs().map(|r| r.line_w).fold(0.0f32, f32::max);
        let mut w = width(&buf);
        if w > max_w {
            let ell_w = size_px * 0.9;
            let cut = buf
                .layout_runs()
                .next()
                .and_then(|run| run.glyphs.iter().filter(|g| g.x + g.w <= max_w - ell_w).map(|g| g.end).max())
                .unwrap_or(0);
            let truncated = format!("{}…", single_line[..cut].trim_end());
            buf.set_text(fs, &truncated, &attrs, Shaping::Advanced);
            buf.shape_until_scroll(fs, false);
            w = width(&buf).min(max_w);
        }
        let pw = w.ceil().max(1.0) as u32;
        let ph = line_h as u32;
        let mut pm = Pixmap::new(pw, ph)?;
        let (stride, height) = (pw as i32, ph as i32);
        let data = pm.data_mut();
        let c = Color::rgba(
            (color.r * 255.0) as u8,
            (color.g * 255.0) as u8,
            (color.b * 255.0) as u8,
            (color.a * 255.0) as u8,
        );
        buf.draw(fs, &mut self.swash, c, |x, y, w, h, col| {
            let a = col.a() as u32;
            if a == 0 {
                return;
            }
            let (sr, sg, sb) = (col.r() as u32 * a / 255, col.g() as u32 * a / 255, col.b() as u32 * a / 255);
            let inv = 255 - a;
            for yy in y.max(0)..(y + h as i32).min(height) {
                for xx in x.max(0)..(x + w as i32).min(stride) {
                    let i = ((yy * stride + xx) * 4) as usize;
                    data[i] = (sr + data[i] as u32 * inv / 255) as u8;
                    data[i + 1] = (sg + data[i + 1] as u32 * inv / 255) as u8;
                    data[i + 2] = (sb + data[i + 2] as u32 * inv / 255) as u8;
                    data[i + 3] = (a + data[i + 3] as u32 * inv / 255) as u8;
                }
            }
        });
        Some(pm)
    }
}

fn draw_selection(pm: &mut Pixmap, cfg: &Config, pal: &Palette, cell: &crate::layout::Cell, rad: f32, t: Transform) {
    use crate::config::SelectionStyle as S;
    let w = cfg.selection_width;
    let r = cell.rect;
    match cfg.selection {
        S::Both => {
            fill_rrect(pm, r, rad, pal.selected, t);
            stroke_rrect(pm, inset(r, w / 2.0), rad - w / 2.0, pal.accent, w, t);
        }
        S::Outline => stroke_rrect(pm, inset(r, w / 2.0), rad - w / 2.0, pal.accent, w, t),
        S::Fill => fill_rrect(pm, r, rad, pal.selected.with_alpha((pal.selected.a * 1.4).min(1.0)), t),
        S::Underline => {
            fill_rrect(pm, r, rad, pal.selected.with_alpha(pal.selected.a * 0.5), t);
            let bar_w = (r.w * 0.4).max(24.0);
            let h = w.max(2.0) + 1.0;
            let bar = Rect { x: r.x + (r.w - bar_w) / 2.0, y: r.y + r.h - h - 1.0, w: bar_w, h };
            fill_rrect(pm, bar, h / 2.0, pal.accent, t);
        }
        S::Glow => {
            for i in (1..=6).rev() {
                let d = i as f32 * 1.5;
                let a = 0.07 * (7 - i) as f32 / 6.0;
                let g = Rect { x: r.x - d, y: r.y - d, w: r.w + 2.0 * d, h: r.h + 2.0 * d };
                stroke_rrect(pm, g, rad + d, pal.accent.with_alpha(a), 3.0, t);
            }
            fill_rrect(pm, r, rad, pal.selected.with_alpha(pal.selected.a * 0.6), t);
            stroke_rrect(pm, inset(r, w / 2.0), rad - w / 2.0, pal.accent, w, t);
        }
    }
}

/// Extra margin (logical px) the panel surface extends beyond the layout's panel rect.
pub fn bleed(cfg: &Config) -> f32 {
    if cfg.scrim > 0.0 { cfg.scrim_feather } else { 0.0 }
}

fn scaled(r: Rect, s: f32) -> Rect {
    Rect { x: r.x * s, y: r.y * s, w: r.w * s, h: r.h * s }
}

/// Signed distance from (px, py) to a rounded rect (negative inside).
fn sdf_rrect(px: f32, py: f32, r: Rect, rad: f32) -> f32 {
    let rad = rad.min(r.w / 2.0).min(r.h / 2.0).max(0.0);
    let qx = (px - (r.x + r.w / 2.0)).abs() - (r.w / 2.0 - rad);
    let qy = (py - (r.y + r.h / 2.0)).abs() - (r.h / 2.0 - rad);
    let (ox, oy) = (qx.max(0.0), qy.max(0.0));
    (ox * ox + oy * oy).sqrt() + qx.max(qy).min(0.0) - rad
}

/// Darken a rounded rect (pixel coordinates) with a smooth falloff over `spread` px outside it.
/// Cheap analytic stand-in for a gaussian shadow: no blur pass needed.
fn darken_rrect(pm: &mut Pixmap, r: Rect, rad: f32, spread: f32, opacity: f32, dy: f32) {
    let (w, h) = (pm.width() as i32, pm.height() as i32);
    let r = Rect { y: r.y + dy, ..r };
    let x0 = ((r.x - spread).floor() as i32).max(0);
    let y0 = ((r.y - spread).floor() as i32).max(0);
    let x1 = ((r.x + r.w + spread).ceil() as i32).min(w);
    let y1 = ((r.y + r.h + spread).ceil() as i32).min(h);
    let data = pm.data_mut();
    for y in y0..y1 {
        for x in x0..x1 {
            let d = sdf_rrect(x as f32 + 0.5, y as f32 + 0.5, r, rad);
            let cov = if d <= 0.0 {
                1.0
            } else if d >= spread {
                continue;
            } else {
                let t = 1.0 - d / spread;
                t * t * (3.0 - 2.0 * t)
            };
            let a = cov * opacity;
            let i = ((y * w + x) * 4) as usize;
            let keep = 1.0 - a;
            for c in &mut data[i..i + 3] {
                *c = (*c as f32 * keep) as u8;
            }
            data[i + 3] = (a * 255.0 + data[i + 3] as f32 * keep).min(255.0) as u8;
        }
    }
}

/// Blurred black copy of a text raster's alpha, padded by `r` px on each side.
fn halo(text: &Pixmap, r: u32, opacity: f32) -> Option<Pixmap> {
    let (w, h) = (text.width() + 2 * r, text.height() + 2 * r);
    let mut a = vec![0f32; (w * h) as usize];
    for y in 0..text.height() {
        for x in 0..text.width() {
            a[((y + r) * w + x + r) as usize] = text.data()[((y * text.width() + x) * 4 + 3) as usize] as f32;
        }
    }
    // Two box-blur passes per axis ≈ gaussian.
    let mut tmp = vec![0f32; a.len()];
    for _ in 0..2 {
        box_blur(&a, &mut tmp, w as usize, h as usize, r as usize, true);
        box_blur(&tmp, &mut a, w as usize, h as usize, r as usize, false);
    }
    let mut pm = Pixmap::new(w, h)?;
    for (px, v) in pm.data_mut().as_chunks_mut::<4>().0.iter_mut().zip(&a) {
        // Boost so the halo reads as a solid edge rather than a faint smudge.
        px[3] = (v * 1.8 * opacity).min(255.0) as u8;
    }
    Some(pm)
}

fn box_blur(src: &[f32], dst: &mut [f32], w: usize, h: usize, r: usize, horizontal: bool) {
    let (n, m) = if horizontal { (h, w) } else { (w, h) };
    let idx = |line: usize, k: usize| if horizontal { line * w + k } else { k * w + line };
    let norm = 1.0 / (2 * r + 1) as f32;
    for line in 0..n {
        let mut acc = 0.0;
        for k in 0..=r.min(m - 1) {
            acc += src[idx(line, k)];
        }
        for k in 0..m {
            dst[idx(line, k)] = acc * norm;
            if k + r + 1 < m {
                acc += src[idx(line, k + r + 1)];
            }
            if k >= r {
                acc -= src[idx(line, k - r)];
            }
        }
    }
}

fn inset(r: Rect, d: f32) -> Rect {
    Rect { x: r.x + d, y: r.y + d, w: (r.w - 2.0 * d).max(0.0), h: (r.h - 2.0 * d).max(0.0) }
}

pub fn rrect_path(r: Rect, rad: f32) -> Option<Path> {
    let rad = rad.max(0.0).min(r.w / 2.0).min(r.h / 2.0);
    let (x, y, w, h) = (r.x, r.y, r.w, r.h);
    let k = 0.552_284_8 * rad;
    let mut pb = PathBuilder::new();
    pb.move_to(x + rad, y);
    pb.line_to(x + w - rad, y);
    pb.cubic_to(x + w - rad + k, y, x + w, y + rad - k, x + w, y + rad);
    pb.line_to(x + w, y + h - rad);
    pb.cubic_to(x + w, y + h - rad + k, x + w - rad + k, y + h, x + w - rad, y + h);
    pb.line_to(x + rad, y + h);
    pb.cubic_to(x + rad - k, y + h, x, y + h - rad + k, x, y + h - rad);
    pb.line_to(x, y + rad);
    pb.cubic_to(x, y + rad - k, x + rad - k, y, x + rad, y);
    pb.close();
    pb.finish()
}

fn paint(c: Rgba) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(c.to_skia());
    p.anti_alias = true;
    p
}

fn fill_rrect(pm: &mut Pixmap, r: Rect, rad: f32, c: Rgba, t: Transform) {
    if let Some(p) = rrect_path(r, rad) {
        pm.fill_path(&p, &paint(c), FillRule::Winding, t, None);
    }
}

fn stroke_rrect(pm: &mut Pixmap, r: Rect, rad: f32, c: Rgba, width: f32, t: Transform) {
    if let Some(p) = rrect_path(r, rad) {
        stroke_path(pm, &p, c, width, t);
    }
}

fn stroke_path(pm: &mut Pixmap, p: &Path, c: Rgba, width: f32, t: Transform) {
    let stroke = Stroke { width, line_cap: LineCap::Round, ..Default::default() };
    pm.stroke_path(p, &paint(c), &stroke, t, None);
}
