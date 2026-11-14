//! Fonts, shaping and glyph rasterization into the shared atlas.
//! Terminal cells take a fast path: (char, style) → cached glyph, no per-frame shaping.
//! UI text (sidebar, headers) is shaped with cosmic-text and cached per string.
use crate::gpu::ATLAS;
use cosmic_text::{Attrs, Buffer, CacheKey, Family, FontSystem, Metrics, Shaping, Style, SwashCache, SwashContent, Weight};
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Clone, Copy)]
pub struct Glyph {
    pub uv: [f32; 4],
    pub left: i32,
    pub top: i32,
    pub color: bool,
}

pub struct Upload {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

/// A shaped UI string: glyphs at integer offsets from the line's top-left.
pub struct Shaped {
    pub glyphs: Vec<(CacheKey, i32, i32, f32)>, // key, x, y (baseline-relative), advance end
    pub width: f32,
    pub ascent: f32,
}

pub struct Fonts {
    fs: FontSystem,
    swash: SwashCache,
    mono: String,
    ui: String,
    pub scale: f32,
    pub mono_pt: f32,
    pub cell_w: f32,
    pub cell_h: f32,
    pub baseline: f32,
    cells: HashMap<(char, u8), Option<(CacheKey, i32, i32)>>,
    glyphs: HashMap<CacheKey, Option<Glyph>>,
    shaped: HashMap<(String, u32, bool, bool), Rc<Shaped>>,
    icons: HashMap<(usize, u32), Option<Glyph>>,
    // Shelf packer state.
    sx: u32,
    sy: u32,
    sh: u32,
    pub uploads: Vec<Upload>,
    /// Atlas filled up this frame: caches were dropped, redraw once more.
    pub overflowed: bool,
}

fn pick(fs: &FontSystem, names: &[&str]) -> Option<String> {
    let db = fs.db();
    names.iter().find(|n| db.faces().any(|f| f.families.iter().any(|(fam, _)| fam == *n))).map(|s| s.to_string())
}

impl Fonts {
    pub fn new(scale: f32) -> Self {
        let fs = FontSystem::new();
        let mono = pick(&fs, &["JetBrains Mono", "SF Mono", "Menlo", "DejaVu Sans Mono", "Consolas", "Liberation Mono"]).unwrap_or_else(|| "monospace".into());
        let ui = pick(&fs, &["Inter", "SF Pro Text", "SF Pro", "Helvetica Neue", "Segoe UI", "Cantarell", "Noto Sans", "DejaVu Sans"]).unwrap_or_else(|| "sans-serif".into());
        let mut f = Fonts {
            fs,
            swash: SwashCache::new(),
            mono,
            ui,
            scale,
            mono_pt: 13.0,
            cell_w: 8.0,
            cell_h: 16.0,
            baseline: 12.0,
            cells: HashMap::new(),
            glyphs: HashMap::new(),
            shaped: HashMap::new(),
            icons: HashMap::new(),
            sx: 0,
            sy: 0,
            sh: 0,
            uploads: vec![],
            overflowed: false,
        };
        f.reset();
        f
    }

    fn attrs<'a>(fam: &'a str, bold: bool, italic: bool) -> Attrs<'a> {
        let fam = match fam {
            "monospace" => Family::Monospace,
            "sans-serif" => Family::SansSerif,
            n => Family::Name(n),
        };
        Attrs::new()
            .family(fam)
            .weight(if bold { Weight::BOLD } else { Weight::NORMAL })
            .style(if italic { Style::Italic } else { Style::Normal })
    }

    /// Recompute cell metrics and drop every cache (scale or font size changed).
    pub fn reset(&mut self) {
        self.cells.clear();
        self.glyphs.clear();
        self.shaped.clear();
        self.icons.clear();
        self.swash = SwashCache::new();
        self.sx = 0;
        self.sy = 0;
        self.sh = 0;
        let px = (self.mono_pt * self.scale).round();
        let line = (px * 1.3).round();
        let fam = self.mono.clone();
        let attrs = Self::attrs(&fam, false, false);
        let mut b = Buffer::new(&mut self.fs, Metrics::new(px, line));
        b.set_text("M", &attrs, Shaping::Advanced, None);
        b.shape_until_scroll(&mut self.fs, false);
        if let Some(run) = b.layout_runs().next() {
            self.baseline = run.line_y.round();
            self.cell_w = run.glyphs.first().map(|g| g.w).unwrap_or(px * 0.6).round().max(1.0);
        }
        self.cell_h = line;
    }

    fn shape(&mut self, s: &str, px: f32, mono: bool, bold: bool, italic: bool) -> Shaped {
        let fam = if mono { self.mono.clone() } else { self.ui.clone() };
        let attrs = Self::attrs(&fam, bold, italic);
        let mut b = Buffer::new(&mut self.fs, Metrics::new(px, (px * 1.3).round()));
        b.set_size(None, None);
        b.set_text(s, &attrs, Shaping::Advanced, None);
        b.shape_until_scroll(&mut self.fs, false);
        let mut out = Shaped { glyphs: vec![], width: 0.0, ascent: 0.0 };
        if let Some(run) = b.layout_runs().next() {
            out.ascent = run.line_y;
            out.width = run.line_w;
            for g in run.glyphs {
                let p = g.physical((0.0, 0.0), 1.0);
                out.glyphs.push((p.cache_key, p.x, p.y, g.x + g.w));
            }
        }
        out
    }

    /// Glyph for one terminal cell. `style`: bit 0 bold, bit 1 italic.
    pub fn cell(&mut self, c: char, style: u8) -> Option<(Glyph, i32, i32)> {
        let k = match self.cells.get(&(c, style)) {
            Some(k) => *k,
            None => {
                let px = (self.mono_pt * self.scale).round();
                let mut buf = [0u8; 4];
                let sh = self.shape(c.encode_utf8(&mut buf), px, true, style & 1 != 0, style & 2 != 0);
                let k = sh.glyphs.first().map(|g| (g.0, g.1, g.2));
                self.cells.insert((c, style), k);
                k
            }
        }?;
        Some((self.glyph(k.0)?, k.1, k.2))
    }

    pub fn ui(&mut self, s: &str, pt: f32, bold: bool, mono: bool) -> Rc<Shaped> {
        let px = (pt * self.scale).round();
        let key = (s.to_string(), px.to_bits(), bold, mono);
        if let Some(r) = self.shaped.get(&key) {
            return r.clone();
        }
        if self.shaped.len() > 4000 {
            self.shaped.clear();
        }
        let r = Rc::new(self.shape(s, px, mono, bold, false));
        self.shaped.insert(key, r.clone());
        r
    }

    pub fn line_h(&self, pt: f32) -> f32 {
        ((pt * self.scale).round() * 1.3).round()
    }

    /// Rasterize (once) and place in the atlas.
    pub fn glyph(&mut self, key: CacheKey) -> Option<Glyph> {
        if let Some(g) = self.glyphs.get(&key) {
            return *g;
        }
        let img = self.swash.get_image_uncached(&mut self.fs, key);
        let g = img.and_then(|img| {
            let color = matches!(img.content, SwashContent::Color);
            let rgba = match img.content {
                SwashContent::Mask => img.data.iter().flat_map(|a| [255, 255, 255, *a]).collect(),
                SwashContent::SubpixelMask => img.data.chunks(4).flat_map(|p| [255, 255, 255, p[1]]).collect(),
                SwashContent::Color => img.data,
            };
            self.place(img.placement.width, img.placement.height, rgba, img.placement.left, img.placement.top, color)
        });
        if !self.overflowed {
            self.glyphs.insert(key, g);
        }
        g
    }

    /// Reserve atlas space and queue the upload.
    fn place(&mut self, w: u32, h: u32, rgba: Vec<u8>, left: i32, top: i32, color: bool) -> Option<Glyph> {
        if w == 0 || h == 0 {
            return None;
        }
        if self.sx + w + 1 > ATLAS {
            self.sx = 0;
            self.sy += self.sh + 1;
            self.sh = 0;
        }
        if self.sy + h + 1 > ATLAS || w + 1 > ATLAS {
            self.overflowed = true;
            return None;
        }
        let (x, y) = (self.sx, self.sy);
        self.sx += w + 1;
        self.sh = self.sh.max(h);
        self.uploads.push(Upload { x, y, w, h, rgba });
        Some(Glyph { uv: [x as f32, y as f32, w as f32, h as f32], left, top, color })
    }

    /// An icon rendered at `pt` points into a square mask. Drawn from its top-left corner.
    pub fn icon(&mut self, icon: &'static Icon, pt: f32) -> Option<Glyph> {
        let px = (pt * self.scale).round() as u32;
        let key = (icon as *const Icon as usize, px);
        if let Some(g) = self.icons.get(&key) {
            return *g;
        }
        let mask = rasterize_icon(icon, px);
        let g = self.place(px, px, mask.iter().flat_map(|a| [255, 255, 255, *a]).collect(), 0, 0, false);
        if !self.overflowed {
            self.icons.insert(key, g);
        }
        g
    }

    /// Called after a frame if the atlas overflowed: start over with an empty atlas.
    pub fn take_overflow(&mut self) -> bool {
        if self.overflowed {
            self.overflowed = false;
            let (w, c, b) = (self.cell_w, self.cell_h, self.baseline);
            self.reset();
            (self.cell_w, self.cell_h, self.baseline) = (w, c, b);
            return true;
        }
        false
    }
}

/// One SVG path of an icon: stroked (`stroke` = width in viewBox units) or filled (`stroke` = 0).
pub struct Layer {
    pub path: &'static str,
    pub stroke: f32,
    pub alpha: f32,
}

/// A monochrome vector icon on a square viewBox, tinted at draw time.
pub struct Icon {
    pub view: f32,
    pub layers: &'static [Layer],
}

const fn stroke(path: &'static str, w: f32) -> Layer {
    Layer { path, stroke: w, alpha: 1.0 }
}

/// Git branch, as Herdr shows for worktrees (lucide-style, Apache-2.0). Circles written as arcs.
pub static ICON_BRANCH: Icon = Icon {
    view: 24.0,
    layers: &[stroke("M4 5.5a2 2 0 1 0 4 0a2 2 0 1 0 -4 0M4 18.5a2 2 0 1 0 4 0a2 2 0 1 0 -4 0M16 7.5a2 2 0 1 0 4 0a2 2 0 1 0 -4 0M6 7.5v9M18 9.5c0 4.5-6.5 3.5-11 7.5", 1.5)],
};

// Folder icons from Zed's assets/icons (folder.svg, folder_open.svg).
const FOLDER_OPEN_BACK: &str = "M5.46293 7.92298C5.46938 7.6844 5.66427 7.49307 5.90369 7.49307H13.7073C13.9467 7.49307 14.1401 7.6844 14.1335 7.92298C14.1 9.16418 13.9102 12.4787 12.8403 12.4787H4.50925C5.57932 12.4787 5.4293 9.16418 5.46293 7.92298Z";

pub static ICON_FOLDER: Icon = Icon {
    view: 16.0,
    layers: &[stroke("M4.41707 12.4786H6.20854H11.5829C12.5724 12.4786 13.3744 11.6766 13.3744 10.6872V7.55212V7.10426C13.3744 6.11486 12.5724 5.3128 11.5829 5.3128H9.63778C9.16265 5.3128 8.70699 5.12406 8.37102 4.78808L7.62898 4.04604C7.29301 3.71007 6.83735 3.52133 6.36222 3.52133H4.41707C3.42768 3.52133 2.62561 4.3234 2.62561 5.3128V10.6872C2.62561 11.6766 3.42768 12.4786 4.41707 12.4786Z", 1.2)],
};

pub static ICON_FOLDER_OPEN: Icon = Icon {
    view: 16.0,
    layers: &[
        Layer { path: FOLDER_OPEN_BACK, stroke: 0.0, alpha: 0.8 },
        stroke(FOLDER_OPEN_BACK, 1.2),
        stroke("M6.0299 12.481H4.0299C2.92533 12.481 2.0299 11.5856 2.0299 10.481L2 5C2 3.89543 2.89543 3 4 3H5.12223C5.65267 3 6.16137 3.21071 6.53645 3.58579L7.36487 4.41421C7.73995 4.78929 8.24865 5 8.77909 5H10.9507C12.0553 5 12.9507 5.89543 12.9507 7V7.5", 1.2),
    ],
};


/// Rasterize all layers (round caps/joins) into one `px`×`px` alpha mask.
pub fn rasterize_icon(icon: &Icon, px: u32) -> Vec<u8> {
    use zeno::{Cap, Fill, Join, Mask, Stroke, Transform};
    let s = px as f32 / icon.view;
    let mut out = vec![0u8; (px * px) as usize];
    for l in icon.layers {
        let mut m = Mask::new(l.path);
        if l.stroke > 0.0 {
            let mut st = Stroke::new(l.stroke);
            st.cap(Cap::Round).join(Join::Round);
            m.style(st);
        } else {
            m.style(Fill::NonZero);
        }
        let (mask, _) = m.transform(Some(Transform::scale(s, s))).size(px, px).render();
        for (o, a) in out.iter_mut().zip(mask) {
            let a = a as f32 * l.alpha / 255.0;
            let b = *o as f32 / 255.0;
            *o = ((b + a * (1.0 - b)) * 255.0).round() as u8;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn branch_icon_ascii() {
        let px = 26;
        for icon in [&super::ICON_BRANCH, &super::ICON_FOLDER, &super::ICON_FOLDER_OPEN] {
        let m = super::rasterize_icon(icon, px);
        assert_eq!(m.len(), (px * px) as usize);
        for y in 0..px {
            let row: String = (0..px).map(|x| match m[(y * px + x) as usize] { 0..=40 => ' ', 41..=150 => '+', _ => '#' }).collect();
            println!("|{row}|");
        }
        assert!(m.iter().any(|a| *a > 200));
        }
    }
}
