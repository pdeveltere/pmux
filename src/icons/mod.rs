//! Monochrome vector icons, rasterized with zeno and tinted at draw time.
//! `files` holds Zed's file-type icons (generated); the UI's own icons are below.
pub mod files;

/// One SVG path of an icon: stroked (`stroke` = width in viewBox units) or filled (`stroke` = 0).
pub struct Layer {
    pub path: &'static str,
    pub stroke: f32,
    pub alpha: f32,
    /// Fill rule: even-odd instead of nonzero.
    pub even_odd: bool,
    /// Stroke cap 0 butt / 1 round / 2 square; join 0 miter / 1 round / 2 bevel.
    pub cap: u8,
    pub join: u8,
    /// SVG matrix (a, b, c, d, e, f) into viewBox units.
    pub xf: [f32; 6],
}

const ID: [f32; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// A monochrome vector icon on a square viewBox, tinted at draw time.
pub struct Icon {
    pub view: f32,
    pub layers: &'static [Layer],
}

const fn stroke(path: &'static str, w: f32) -> Layer {
    Layer { path, stroke: w, alpha: 1.0, even_odd: false, cap: 1, join: 1, xf: ID }
}

/// Git branch, as Herdr shows for worktrees (lucide-style, Apache-2.0). Circles written as arcs.
pub static BRANCH: Icon = Icon {
    view: 24.0,
    layers: &[stroke("M4 5.5a2 2 0 1 0 4 0a2 2 0 1 0 -4 0M4 18.5a2 2 0 1 0 4 0a2 2 0 1 0 -4 0M16 7.5a2 2 0 1 0 4 0a2 2 0 1 0 -4 0M6 7.5v9M18 9.5c0 4.5-6.5 3.5-11 7.5", 1.5)],
};

/// Symlink marker, Zed's arrow_up_right.svg.
pub static ARROW_UP_RIGHT: Icon = Icon { view: 16.0, layers: &[stroke("M4.5 11.5L11.5 4.5M4.5 4.5L11.5 4.5L11.5 11.5", 1.2)] };

/// Status bar panel toggles (lucide panel-left / bot / panel-right, ISC). Rounded rects written as arcs.
const FRAME: &str = "M5 3h14a2 2 0 0 1 2 2v14a2 2 0 0 1 -2 2h-14a2 2 0 0 1 -2 -2v-14a2 2 0 0 1 2 -2z";
pub static PANEL_LEFT: Icon = Icon { view: 24.0, layers: &[stroke(FRAME, 1.5), stroke("M9 3v18", 1.5)] };
pub static PANEL_RIGHT: Icon = Icon { view: 24.0, layers: &[stroke(FRAME, 1.5), stroke("M15 3v18", 1.5)] };
pub static BOT: Icon = Icon {
    view: 24.0,
    layers: &[stroke("M12 8V4H8M6 8h12a2 2 0 0 1 2 2v8a2 2 0 0 1 -2 2h-12a2 2 0 0 1 -2 -2v-8a2 2 0 0 1 2 -2zM2 14h2M20 14h2M15 13v2M9 13v2", 1.5)],
};

// Folder icons from Zed's assets/icons (folder.svg, folder_open.svg).
const FOLDER_OPEN_BACK: &str = "M5.46293 7.92298C5.46938 7.6844 5.66427 7.49307 5.90369 7.49307H13.7073C13.9467 7.49307 14.1401 7.6844 14.1335 7.92298C14.1 9.16418 13.9102 12.4787 12.8403 12.4787H4.50925C5.57932 12.4787 5.4293 9.16418 5.46293 7.92298Z";

const FOLDER_SHAPE: &str = "M4.41707 12.4786H6.20854H11.5829C12.5724 12.4786 13.3744 11.6766 13.3744 10.6872V7.55212V7.10426C13.3744 6.11486 12.5724 5.3128 11.5829 5.3128H9.63778C9.16265 5.3128 8.70699 5.12406 8.37102 4.78808L7.62898 4.04604C7.29301 3.71007 6.83735 3.52133 6.36222 3.52133H4.41707C3.42768 3.52133 2.62561 4.3234 2.62561 5.3128V10.6872C2.62561 11.6766 3.42768 12.4786 4.41707 12.4786Z";

/// Filled like the open folder's front: a 0.8 fill under the outline.
pub static FOLDER: Icon = Icon {
    view: 16.0,
    layers: &[Layer { path: FOLDER_SHAPE, stroke: 0.0, alpha: 0.8, even_odd: false, cap: 1, join: 1, xf: ID }, stroke(FOLDER_SHAPE, 1.2)],
};

pub static FOLDER_OPEN: Icon = Icon {
    view: 16.0,
    layers: &[
        Layer { path: FOLDER_OPEN_BACK, stroke: 0.0, alpha: 0.8, even_odd: false, cap: 1, join: 1, xf: ID },
        stroke(FOLDER_OPEN_BACK, 1.2),
        stroke("M6.0299 12.481H4.0299C2.92533 12.481 2.0299 11.5856 2.0299 10.481L2 5C2 3.89543 2.89543 3 4 3H5.12223C5.65267 3 6.16137 3.21071 6.53645 3.58579L7.36487 4.41421C7.73995 4.78929 8.24865 5 8.77909 5H10.9507C12.0553 5 12.9507 5.89543 12.9507 7V7.5", 1.2),
    ],
};



/// Rasterize all layers (round caps/joins) into one `px`×`px` alpha mask.
pub fn rasterize(icon: &Icon, px: u32) -> Vec<u8> {
    use zeno::{Cap, Fill, Join, Mask, Stroke, Transform};
    let s = px as f32 / icon.view;
    let mut out = vec![0u8; (px * px) as usize];
    for l in icon.layers {
        let mut m = Mask::new(l.path);
        if l.stroke > 0.0 {
            let mut st = Stroke::new(l.stroke);
            st.cap([Cap::Butt, Cap::Round, Cap::Square][l.cap as usize]).join([Join::Miter, Join::Round, Join::Bevel][l.join as usize]);
            m.style(st);
        } else {
            m.style(if l.even_odd { Fill::EvenOdd } else { Fill::NonZero });
        }
        let [a, b, c, d, e, f] = l.xf;
        let xf = Transform::new(a, b, c, d, e, f).then(&Transform::scale(s, s));
        let (mask, _) = m.transform(Some(xf)).size(px, px).render();
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
    use super::*;

    fn ascii(m: &[u8], px: usize) {
        for y in 0..px {
            println!("|{}|", (0..px).map(|x| match m[y * px + x] { 0..=40 => ' ', 41..=150 => '+', _ => '#' }).collect::<String>());
        }
    }

    #[test]
    fn ui_icons_render() {
        let px = 26;
        for icon in [&BRANCH, &FOLDER, &FOLDER_OPEN, &ARROW_UP_RIGHT, &PANEL_LEFT, &PANEL_RIGHT, &BOT] {
            let m = rasterize(icon, px);
            assert_eq!(m.len(), (px * px) as usize);
            ascii(&m, px as usize);
            assert!(m.iter().any(|a| *a > 200));
        }
    }

    #[test]
    fn zed_file_icons() {
        use files::*;
        let is = |name: &str, icon: &'static Icon| std::ptr::eq(for_name(name), icon);
        assert!(is("main.rs", &RUST) && is("Cargo.toml", &TOML) && is("Dockerfile", &DOCKER) && is("docker-compose.yml", &DOCKER));
        assert!(is("index.d.ts", &TYPESCRIPT) && is(".gitignore", &GIT) && is("README.md", &BOOK) && is(".env", &FILE) && is("x.json", &CODE));
        for icon in ALL {
            let m = rasterize(icon, 32);
            assert!(m.iter().filter(|a| **a > 128).count() > 20, "icon renders");
        }
        for icon in [&RUST, &HELM, &KDL, &VYPER] {
            ascii(&rasterize(icon, 24), 24);
        }
    }
}
