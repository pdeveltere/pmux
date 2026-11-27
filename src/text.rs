//! Fonts, shaping and glyph rasterization into the shared atlas.
//! Terminal cells take a fast path: (char, style) → cached glyph, no per-frame shaping.
//! UI text (sidebar, headers) is shaped with cosmic-text and cached per string.
use crate::gpu::{ATLAS, ATLAS_MAX};
use crate::icons::{self, Icon};
use cosmic_text::{
    Attrs, Buffer, CacheKey, Family, FontSystem, Metrics, Shaping, Style, SwashCache, SwashContent, Weight,
};
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Clone, Copy)]
pub struct Glyph {
    pub uv: [f32; 4],
    pub left: i32,
    pub top: i32,
    pub color: bool,
}

/// RGBA pixels waiting to be copied into the atlas at (x, y) by the renderer.
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

/// A terminal cell's glyph: cache key and its offset within the cell.
type CellGlyph = (CacheKey, i32, i32);

/// Font state and every glyph cache. All caches and the atlas are dropped together on `reset`
/// (zoom, scale change) or when the atlas overflows.
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
    /// (char, style) → glyph key and offset; `None` when no font has it.
    cells: HashMap<(char, u8), Option<CellGlyph>>,
    /// Fast path for ASCII cells, indexed `style * 128 + char`: the finished glyph, so drawing a
    /// terminal of plain text does no hashing. Outer `None` = not looked up yet.
    ascii: Vec<Option<Option<(Glyph, i32, i32)>>>,
    glyphs: HashMap<CacheKey, Option<Glyph>>,
    /// Shaped UI strings by (px, bold, mono), then text: looked up by `&str`, so a cache hit
    /// doesn't allocate. `shaped_n` counts the strings, to bound the cache.
    shaped: HashMap<(u32, bool, bool), HashMap<String, Rc<Shaped>>>,
    shaped_n: usize,
    icons: HashMap<(usize, u32), Option<Glyph>>,
    /// Atlas side in texels (the renderer matches its texture to it).
    pub atlas: u32,
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
    names
        .iter()
        .find(|n| db.faces().any(|f| f.families.iter().any(|(fam, _)| fam == *n)))
        .map(std::string::ToString::to_string)
}

/// Zed's default fonts (bundled, OFL): Lilex for buffer/terminal, IBM Plex Sans for UI.
const BUNDLED: &[&[u8]] = &[
    include_bytes!("../assets/fonts/Lilex-Regular.ttf"),
    include_bytes!("../assets/fonts/Lilex-Bold.ttf"),
    include_bytes!("../assets/fonts/Lilex-Italic.ttf"),
    include_bytes!("../assets/fonts/Lilex-BoldItalic.ttf"),
    include_bytes!("../assets/fonts/IBMPlexSans-Regular.ttf"),
    include_bytes!("../assets/fonts/IBMPlexSans-SemiBold.ttf"),
];

/// Zed's default `buffer_font_size` (terminal inherits it).
pub const MONO_PT: f32 = 15.0;
/// Zed's `ui_font_size` is 16 and its default label is 0.875rem = 14px; our base label is 13pt.
const UI_SCALE: f32 = 16.0 * 0.875 / 13.0;

impl Fonts {
    pub fn new(scale: f32) -> Self {
        let mut fs = FontSystem::new();
        for f in BUNDLED {
            fs.db_mut().load_font_data(f.to_vec());
        }
        let mono = pick(
            &fs,
            &["Lilex", "JetBrains Mono", "SF Mono", "Menlo", "DejaVu Sans Mono", "Consolas", "Liberation Mono"],
        )
        .unwrap_or_else(|| "monospace".into());
        let ui = pick(
            &fs,
            &[
                "IBM Plex Sans",
                "Inter",
                "SF Pro Text",
                "SF Pro",
                "Helvetica Neue",
                "Segoe UI",
                "Cantarell",
                "Noto Sans",
                "DejaVu Sans",
            ],
        )
        .unwrap_or_else(|| "sans-serif".into());
        let mut f = Fonts {
            fs,
            swash: SwashCache::new(),
            mono,
            ui,
            scale,
            mono_pt: MONO_PT,
            cell_w: 8.0,
            cell_h: 16.0,
            baseline: 12.0,
            cells: HashMap::new(),
            ascii: vec![None; 4 * 128],
            glyphs: HashMap::new(),
            shaped: HashMap::new(),
            shaped_n: 0,
            icons: HashMap::new(),
            atlas: ATLAS,
            sx: 0,
            sy: 0,
            sh: 0,
            uploads: vec![],
            overflowed: false,
        };
        f.reset();
        f
    }

    fn attrs(fam: &str, bold: bool, italic: bool) -> Attrs<'_> {
        let fam = match fam {
            "monospace" => Family::Monospace,
            "sans-serif" => Family::SansSerif,
            n => Family::Name(n),
        };
        Attrs::new().family(fam).weight(if bold { Weight::BOLD } else { Weight::NORMAL }).style(if italic {
            Style::Italic
        } else {
            Style::Normal
        })
    }

    /// Recompute cell metrics and drop every cache (scale or font size changed).
    pub fn reset(&mut self) {
        self.cells.clear();
        self.ascii.fill(None);
        self.glyphs.clear();
        self.shaped.clear();
        self.shaped_n = 0;
        self.icons.clear();
        self.swash = SwashCache::new();
        self.sx = 0;
        self.sy = 0;
        self.sh = 0;
        self.uploads.clear();
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
        let slot = (c.is_ascii() && style < 4).then(|| style as usize * 128 + c as usize);
        if let Some(hit) = slot.and_then(|i| self.ascii[i]) {
            return hit;
        }
        let g = self.cell_slow(c, style);
        if let (Some(i), false) = (slot, self.overflowed) {
            self.ascii[i] = Some(g);
        }
        g
    }

    fn cell_slow(&mut self, c: char, style: u8) -> Option<(Glyph, i32, i32)> {
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
        let px = (pt * UI_SCALE * self.scale).round();
        let key = (px.to_bits(), bold, mono);
        if let Some(r) = self.shaped.get(&key).and_then(|m| m.get(s)) {
            return r.clone();
        }
        if self.shaped_n > 4000 {
            self.shaped.clear();
            self.shaped_n = 0;
        }
        let r = Rc::new(self.shape(s, px, mono, bold, false));
        self.shaped.entry(key).or_default().insert(s.to_string(), r.clone());
        self.shaped_n += 1;
        r
    }

    pub fn line_h(&self, pt: f32) -> f32 {
        ((pt * UI_SCALE * self.scale).round() * 1.3).round()
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
        if self.sx + w + 1 > self.atlas {
            self.sx = 0;
            self.sy += self.sh + 1;
            self.sh = 0;
        }
        if self.sy + h + 1 > self.atlas || w + 1 > self.atlas {
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
        let key = (std::ptr::from_ref::<Icon>(icon) as usize, px);
        if let Some(g) = self.icons.get(&key) {
            return *g;
        }
        let mask = icons::rasterize(icon, px);
        let g = self.place(px, px, mask.iter().flat_map(|a| [255, 255, 255, *a]).collect(), 0, 0, false);
        if !self.overflowed {
            self.icons.insert(key, g);
        }
        g
    }

    /// Called after a frame if the atlas overflowed: start over with an empty, bigger atlas.
    pub fn take_overflow(&mut self) -> bool {
        if self.overflowed {
            self.overflowed = false;
            self.atlas = (self.atlas * 2).min(ATLAS_MAX);
            let (w, c, b) = (self.cell_w, self.cell_h, self.baseline);
            self.reset();
            (self.cell_w, self.cell_h, self.baseline) = (w, c, b);
            return true;
        }
        false
    }
}
