//! Windows-style switcher grid: uniform-height thumbnails whose widths follow each window's
//! aspect ratio, wrapped into centered rows that shrink until everything fits on screen.

use crate::config::Config;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && py >= self.y && px < self.x + self.w && py < self.y + self.h
    }
    pub fn offset(&self, dx: f32, dy: f32) -> Rect {
        Rect { x: self.x + dx, y: self.y + dy, ..*self }
    }
    pub fn center_x(&self) -> f32 {
        self.x + self.w / 2.0
    }
}

#[derive(Clone, Debug)]
pub struct Cell {
    /// Whole cell (selection highlight), panel-local.
    pub rect: Rect,
    /// Icon + title strip, panel-local.
    pub header: Rect,
    /// Preview rect fitted to the window's aspect ratio inside `slot`, panel-local, pixel-snapped.
    pub thumb: Rect,
    /// Close button (shown on the selected cell), panel-local.
    pub close: Rect,
}

#[derive(Clone, Debug)]
pub struct Layout {
    /// Panel rect in output (root surface) logical coordinates, pixel-snapped.
    pub panel: Rect,
    pub cells: Vec<Cell>,
    pub rows: Vec<std::ops::Range<usize>>,
}

pub const PANEL_PAD: f32 = 16.0;
pub const CELL_PAD: f32 = 8.0;
pub const GAP: f32 = 6.0;

pub fn header_height(cfg: &Config) -> f32 {
    (cfg.icon_size.max(cfg.font_size * 1.45) + 8.0).round()
}

/// `aspects[i]` = width / height of window i.
pub fn compute(cfg: &Config, out_w: f32, out_h: f32, aspects: &[f32]) -> Layout {
    let header = header_height(cfg);
    let max_w = (out_w * cfg.max_width_fraction).max(200.0);
    let max_h = (out_h * cfg.max_height_fraction).max(150.0);
    let inner_max = max_w - 2.0 * PANEL_PAD;

    let mut th = cfg.thumbnail_height;
    let (widths, rows) = loop {
        let widths: Vec<f32> = aspects
            .iter()
            .map(|&a| {
                let a = if a.is_finite() && a > 0.0 { a } else { 16.0 / 9.0 };
                // Keep very tall/narrow windows readable and very wide ones bounded.
                ((th * a).clamp(th * 0.75, th * 2.6) + 2.0 * CELL_PAD).min(inner_max).round()
            })
            .collect();
        let rows = balanced_rows(&widths, inner_max);
        let cell_h = 2.0 * CELL_PAD + header + th;
        let total_h = rows.len() as f32 * cell_h + (rows.len().saturating_sub(1)) as f32 * GAP + 2.0 * PANEL_PAD;
        if total_h <= max_h || th <= cfg.min_thumbnail_height {
            break (widths, rows);
        }
        th = (th * 0.92).max(cfg.min_thumbnail_height).floor();
    };

    let cell_h = (2.0 * CELL_PAD + header + th).round();
    let row_w = |r: &std::ops::Range<usize>| widths[r.clone()].iter().sum::<f32>() + GAP * (r.len().saturating_sub(1)) as f32;
    let inner_w = rows.iter().map(row_w).fold(0.0f32, f32::max).max(160.0);
    let panel_w = (inner_w + 2.0 * PANEL_PAD).round();
    let panel_h = (rows.len() as f32 * cell_h + rows.len().saturating_sub(1) as f32 * GAP + 2.0 * PANEL_PAD).round();
    let panel = Rect {
        x: ((out_w - panel_w) / 2.0).round().max(0.0),
        y: ((out_h - panel_h) / 2.0).round().max(0.0),
        w: panel_w,
        h: panel_h,
    };

    let mut cells = Vec::with_capacity(aspects.len());
    for (ri, r) in rows.iter().enumerate() {
        let mut x = (PANEL_PAD + (inner_w - row_w(r)) / 2.0).round();
        let y = PANEL_PAD + ri as f32 * (cell_h + GAP);
        for i in r.clone() {
            let rect = Rect { x, y, w: widths[i], h: cell_h };
            let header_r = Rect { x: x + CELL_PAD, y: y + CELL_PAD, w: widths[i] - 2.0 * CELL_PAD, h: header };
            let slot = Rect { x: x + CELL_PAD, y: y + CELL_PAD + header, w: widths[i] - 2.0 * CELL_PAD, h: th };
            let thumb = fit(slot, aspects[i]);
            let cs = (header - 6.0).max(14.0);
            let close = Rect { x: header_r.x + header_r.w - cs, y: header_r.y + (header - cs) / 2.0 - 2.0, w: cs, h: cs };
            cells.push(Cell { rect, header: header_r, thumb, close });
            x += widths[i] + GAP;
        }
    }
    Layout { panel, cells, rows }
}

/// Greedy packing decides how many rows are needed; then find the narrowest row limit that still
/// fits in that many rows, so rows come out evenly filled (3+3 rather than 5+1).
fn balanced_rows(widths: &[f32], max: f32) -> Vec<std::ops::Range<usize>> {
    let greedy = pack_rows(widths, max);
    let n = greedy.len();
    if n <= 1 {
        return greedy;
    }
    let total: f32 = widths.iter().sum::<f32>() + GAP * (widths.len() - 1) as f32;
    let widest = widths.iter().copied().fold(0.0, f32::max);
    let (mut lo, mut hi) = ((total / n as f32).max(widest), max);
    for _ in 0..24 {
        let mid = (lo + hi) / 2.0;
        if pack_rows(widths, mid).len() <= n {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    pack_rows(widths, hi)
}

fn pack_rows(widths: &[f32], max: f32) -> Vec<std::ops::Range<usize>> {
    let mut rows = Vec::new();
    let mut start = 0;
    let mut w = 0.0;
    for (i, &cw) in widths.iter().enumerate() {
        let next = if i == start { cw } else { w + GAP + cw };
        if i > start && next > max {
            rows.push(start..i);
            start = i;
            w = cw;
        } else {
            w = next;
        }
    }
    if start < widths.len() {
        rows.push(start..widths.len());
    }
    rows
}

/// Largest rect with the given aspect ratio centered inside `slot`, snapped to whole pixels.
pub fn fit(slot: Rect, aspect: f32) -> Rect {
    let a = if aspect.is_finite() && aspect > 0.0 { aspect } else { 16.0 / 9.0 };
    let (w, h) = if slot.w / slot.h > a { (slot.h * a, slot.h) } else { (slot.w, slot.w / a) };
    let w = w.round().max(1.0);
    let h = h.round().max(1.0);
    Rect { x: (slot.x + (slot.w - w) / 2.0).round(), y: (slot.y + (slot.h - h) / 2.0).round(), w, h }
}

impl Layout {
    pub fn row_of(&self, idx: usize) -> usize {
        self.rows.iter().position(|r| r.contains(&idx)).unwrap_or(0)
    }

    /// Index of the cell in the row `delta` rows away whose center is closest horizontally.
    pub fn vertical_neighbor(&self, idx: usize, delta: isize) -> usize {
        if self.rows.is_empty() || idx >= self.cells.len() {
            return idx;
        }
        let n = self.rows.len() as isize;
        let row = (self.row_of(idx) as isize + delta).rem_euclid(n) as usize;
        let cx = self.cells[idx].rect.center_x();
        self.rows[row]
            .clone()
            .min_by(|&a, &b| {
                let da = (self.cells[a].rect.center_x() - cx).abs();
                let db = (self.cells[b].rect.center_x() - cx).abs();
                da.total_cmp(&db)
            })
            .unwrap_or(idx)
    }

    /// Hit test in root-surface coordinates.
    pub fn hit(&self, x: f32, y: f32) -> Option<usize> {
        let (px, py) = (x - self.panel.x, y - self.panel.y);
        self.cells.iter().position(|c| c.rect.contains(px, py))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_on_screen() {
        let cfg = Config::default();
        for n in [1usize, 2, 5, 12, 40, 120] {
            let aspects: Vec<f32> = (0..n).map(|i| [1.78, 1.33, 0.6, 2.4][i % 4]).collect();
            let l = compute(&cfg, 1920.0, 1080.0, &aspects);
            assert_eq!(l.cells.len(), n);
            assert!(l.panel.w <= 1920.0 * cfg.max_width_fraction + 1.0, "n={n} w={}", l.panel.w);
            for c in &l.cells {
                assert!(c.rect.x >= 0.0 && c.rect.x + c.rect.w <= l.panel.w + 0.5);
                assert!(c.rect.x <= c.thumb.x && c.thumb.x + c.thumb.w <= c.rect.x + c.rect.w + 0.5);
            }
        }
    }

    #[test]
    fn neighbors() {
        let cfg = Config::default();
        let l = compute(&cfg, 1000.0, 2000.0, &[1.78; 9]);
        assert!(l.rows.len() > 1);
        let below = l.vertical_neighbor(0, 1);
        assert_eq!(l.row_of(below), 1);
        assert_eq!(l.vertical_neighbor(below, -1), 0);
    }
}

#[cfg(test)]
mod balance_tests {
    use super::*;

    #[test]
    fn rows_are_balanced() {
        let widths = [300.0; 6];
        let rows = balanced_rows(&widths, 1600.0); // greedy would give 5 + 1
        assert_eq!(rows, vec![0..3, 3..6]);
        assert_eq!(balanced_rows(&widths, 2000.0), vec![0..6]);
    }
}
