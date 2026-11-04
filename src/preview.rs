//! Pictures of changed image files, shown in the diff instead of "Binary file".
//!
//! Reading and decoding happen in the background, with the rest of the diff;
//! the decoded pixels become a texture the first time they're drawn.

use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use egui::{Color32, ColorImage, Painter, Rect, Stroke, TextureHandle, TextureOptions, Ui, Vec2, pos2, vec2};
use resvg::{tiny_skia, usvg};

use crate::git::{self, Change, DiffFile};
use crate::views::{DIFF_ROW, DiffMode};

/// Image files bigger than this aren't read.
pub const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;
/// At most this many pictures are shown in one diff (the rest say why not).
pub const MAX_PREVIEWS: usize = 60;
/// Pictures are kept no bigger than this (pixels on the long side) once read.
const MAX_DECODED: u32 = 1024;
/// How much room one picture gets in the diff.
pub const BOX_W: f32 = 480.0;
pub const BOX_H: f32 = 420.0;
/// Tiny pictures (icons) are blown up to about this size, in whole steps.
const SMALL: f32 = 96.0;

const RASTER: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp", "ico"];

fn ext(path: &str) -> String {
    Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}

fn is_svg(path: &str) -> bool {
    ext(path) == "svg"
}

/// Whether a file's name says it's a picture Headway can show.
pub fn is_image_path(path: &str) -> bool {
    let e = ext(path);
    e == "svg" || RASTER.contains(&e.as_str())
}

/// How much a picture of this size is scaled to fit its box in the diff.
fn fit_scale([w, h]: [usize; 2]) -> f32 {
    if w == 0 || h == 0 {
        return 1.0;
    }
    let (w, h) = (w as f32, h as f32);
    let fit = (BOX_W / w).min(BOX_H / h);
    if fit < 1.0 {
        fit
    } else {
        // Small pictures grow in whole steps so pixel art stays crisp.
        (SMALL / w.max(h)).floor().clamp(1.0, fit.floor().max(1.0))
    }
}

/// One side (before or after) of a changed picture.
pub struct ImageSide {
    pub label: &'static str,
    /// Size of the picture as stored in the file.
    pub size: [usize; 2],
    /// Why it can't be shown, if so.
    pub error: Option<String>,
    /// Decoded pixels, handed to the GPU (and dropped here) on first draw.
    pixels: Mutex<Option<ColorImage>>,
    tex: OnceLock<Option<TextureHandle>>,
}

impl ImageSide {
    fn ok(label: &'static str, size: [usize; 2], img: ColorImage) -> Self {
        Self { label, size, error: None, pixels: Mutex::new(Some(img)), tex: OnceLock::new() }
    }

    fn failed(label: &'static str, error: String) -> Self {
        Self { label, size: [0, 0], error: Some(error), pixels: Mutex::new(None), tex: OnceLock::new() }
    }

    /// How much blowing up (or shrinking) the picture gets when drawn.
    pub fn scale(&self) -> f32 {
        fit_scale(self.size)
    }

    /// Size on screen (zero when it can't be shown).
    pub fn display_size(&self) -> Vec2 {
        if self.error.is_some() {
            return Vec2::ZERO;
        }
        vec2(self.size[0] as f32, self.size[1] as f32) * self.scale()
    }

    /// The picture's texture, made the first time it's needed.
    pub fn texture(&self, ctx: &egui::Context, name: &str) -> Option<&TextureHandle> {
        self.tex
            .get_or_init(|| {
                let img = self.pixels.lock().ok()?.take()?;
                let opts = if self.scale() > 1.0 { TextureOptions::NEAREST } else { TextureOptions::LINEAR };
                Some(ctx.load_texture(name, img, opts))
            })
            .as_ref()
    }
}

/// The picture(s) shown for one changed file.
pub struct ImagePreview {
    pub sides: Vec<ImageSide>,
}

impl ImagePreview {
    /// Diff rows it takes: a title row, the pictures, and a little space.
    pub fn rows(&self) -> usize {
        let h = self.sides.iter().map(|s| if s.error.is_some() { DIFF_ROW } else { s.display_size().y }).fold(0.0, f32::max);
        1 + (h / DIFF_ROW).ceil() as usize + 1
    }
}

/// Draw a picture filling `full`, over a checkerboard (so see-through parts show
/// as such) and inside a thin border. Only what's inside the painter's clip is drawn.
pub fn paint(p: &Painter, ctx: &egui::Context, side: &ImageSide, full: Rect, name: &str) {
    const SQ: f32 = 8.0;
    let area = full.intersect(p.clip_rect());
    if !area.is_positive() {
        return;
    }
    let (c0, c1) = (Color32::from_gray(58), Color32::from_gray(44));
    let mut row = ((area.top() - full.top()) / SQ).floor();
    while full.top() + row * SQ < area.bottom() {
        let mut col = ((area.left() - full.left()) / SQ).floor();
        while full.left() + col * SQ < area.right() {
            let sq = Rect::from_min_size(pos2(full.left() + col * SQ, full.top() + row * SQ), vec2(SQ, SQ)).intersect(full);
            p.rect_filled(sq, 0.0, if (row + col) as i64 % 2 == 0 { c0 } else { c1 });
            col += 1.0;
        }
        row += 1.0;
    }
    if let Some(tex) = side.texture(ctx, name) {
        p.image(tex.id(), full, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    }
    p.rect_stroke(full, 0.0, Stroke::new(1.0, crate::theme::BORDER), egui::StrokeKind::Outside);
}

/// Lay out a picture no wider than `max_w` (or its error text) in an ordinary layout.
pub fn show(ui: &mut Ui, side: &ImageSide, max_w: f32, name: &str) {
    if let Some(e) = &side.error {
        ui.label(egui::RichText::new(e).color(crate::theme::FAINT));
        return;
    }
    let size = side.display_size();
    let size = size * (max_w / size.x).min(1.0);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    paint(ui.painter(), ui.ctx(), side, rect, name);
}

/// Every version of a conflicted picture; `None` where that side has no file.
pub struct ConflictPictures {
    pub base: Option<ImageSide>,
    pub ours: Option<ImageSide>,
    pub theirs: Option<ImageSide>,
}

/// The versions of a conflicted picture, from the index stages git keeps during
/// a conflict (1 = common ancestor, 2 = ours, 3 = theirs). `None` if it isn't a picture.
pub fn load_conflict(repo: &Path, path: &str) -> Option<ConflictPictures> {
    if !is_image_path(path) {
        return None;
    }
    let svg = is_svg(path);
    let stage = |n: u8| {
        // A side that can't be read doesn't have the file (it was deleted there).
        let bytes = read(repo, &Source::Object(format!(":{n}:{path}"))).ok()?;
        Some(match decode(&bytes, svg) {
            Ok((size, img)) => ImageSide::ok("", size, img),
            Err(e) => ImageSide::failed("", e),
        })
    };
    Some(ConflictPictures { base: stage(1), ours: stage(2), theirs: stage(3) })
}

/// Where one side's bytes come from.
enum Source {
    /// A git object, as `<rev>:<path>` (or `:<path>` for the index).
    Object(String),
    /// The file in the working folder.
    Disk(String),
}

fn read(repo: &Path, src: &Source) -> Result<Vec<u8>, String> {
    let bytes = match src {
        Source::Object(spec) => git::run_bytes(repo, &["cat-file", "blob", spec])?,
        Source::Disk(p) => {
            let full = repo.join(p);
            let len = std::fs::metadata(&full).map_err(|e| e.to_string())?.len();
            if len as usize > MAX_IMAGE_BYTES {
                return Err("Too large to show".into());
            }
            std::fs::read(&full).map_err(|e| e.to_string())?
        }
    };
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err("Too large to show".into());
    }
    Ok(bytes)
}

/// Turn a file's bytes into pixels: (size as stored, pixels to draw).
pub fn decode(bytes: &[u8], svg: bool) -> Result<([usize; 2], ColorImage), String> {
    if svg {
        return decode_svg(bytes);
    }
    let img = image::load_from_memory(bytes).map_err(|e| format!("Can't read this picture ({e})"))?;
    let size = [img.width() as usize, img.height() as usize];
    let img = if img.width().max(img.height()) > MAX_DECODED { img.thumbnail(MAX_DECODED, MAX_DECODED) } else { img };
    let rgba = img.to_rgba8();
    Ok((size, ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], rgba.as_raw())))
}

fn fonts() -> Arc<usvg::fontdb::Database> {
    static FONTS: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    FONTS
        .get_or_init(|| {
            let mut db = usvg::fontdb::Database::new();
            db.load_system_fonts();
            Arc::new(db)
        })
        .clone()
}

fn decode_svg(bytes: &[u8]) -> Result<([usize; 2], ColorImage), String> {
    let opt = usvg::Options { fontdb: fonts(), ..Default::default() };
    let tree = usvg::Tree::from_data(bytes, &opt).map_err(|e| format!("Can't read this drawing ({e})"))?;
    let s = tree.size();
    let size = [s.width().round().max(1.0) as usize, s.height().round().max(1.0) as usize];
    // Draw it at twice the size it's shown at (sharp on high-DPI screens), within limits.
    let k = (fit_scale(size) * 2.0).min(MAX_DECODED as f32 / s.width().max(s.height()));
    let (w, h) = ((s.width() * k).round().max(1.0) as u32, (s.height() * k).round().max(1.0) as u32);
    let mut pixmap = tiny_skia::Pixmap::new(w, h).ok_or("Drawing has no size")?;
    resvg::render(&tree, tiny_skia::Transform::from_scale(k, k), &mut pixmap.as_mut());
    Ok((size, ColorImage::from_rgba_premultiplied([w as usize, h as usize], pixmap.data())))
}

fn side(repo: &Path, label: &'static str, src: Source, svg: bool) -> ImageSide {
    match read(repo, &src).and_then(|b| decode(&b, svg)) {
        Ok((size, img)) => ImageSide::ok(label, size, img),
        Err(e) => ImageSide::failed(label, e),
    }
}

/// The picture(s) for one changed file, or `None` if it isn't a picture.
/// `commit` is the commit being shown, for `DiffMode::Commit`.
pub fn load(repo: &Path, f: &DiffFile, mode: DiffMode, commit: Option<&str>) -> Option<ImagePreview> {
    if !is_image_path(&f.path) || matches!(f.change, Change::Conflict(_)) {
        return None;
    }
    // Placeholders stand for files that weren't read; only new files are worth a look anyway.
    if f.note.is_some() && f.change != Change::Untracked {
        return None;
    }
    let svg = is_svg(&f.path);
    let old_path = f.old_path.as_deref().unwrap_or(&f.path);
    let (old, new) = match mode {
        DiffMode::Commit => {
            let c = commit?;
            (Source::Object(format!("{c}^:{old_path}")), Source::Object(format!("{c}:{}", f.path)))
        }
        DiffMode::Staged => (Source::Object(format!("HEAD:{old_path}")), Source::Object(format!(":{}", f.path))),
        DiffMode::Unstaged | DiffMode::Untracked => (Source::Object(format!(":{old_path}")), Source::Disk(f.path.clone())),
    };
    let sides = match (&f.change, mode) {
        (Change::Untracked, _) | (_, DiffMode::Untracked) | (Change::Added, _) => vec![side(repo, "Added", new, svg)],
        (Change::Deleted, _) => vec![side(repo, "Deleted", old, svg)],
        _ => vec![side(repo, "Before", old, svg), side(repo, "After", new, svg)],
    };
    Some(ImagePreview { sides })
}

/// Pictures for every image file in a diff (`modes` says what each file is).
pub fn load_all(repo: &Path, files: &[DiffFile], modes: &[DiffMode], commit: Option<&str>) -> Vec<Option<ImagePreview>> {
    let mut shown = 0;
    files
        .iter()
        .zip(modes)
        .map(|(f, &m)| {
            if !is_image_path(&f.path) || shown >= MAX_PREVIEWS {
                return None;
            }
            let p = load(repo, f, m, commit);
            shown += p.is_some() as usize;
            p
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba([200, 30, 30, 255]));
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    #[test]
    fn knows_picture_names() {
        assert!(is_image_path("a/b/logo.PNG"));
        assert!(is_image_path("x.jpeg") && is_image_path("x.svg") && is_image_path("x.webp"));
        assert!(!is_image_path("main.rs") && !is_image_path("png"));
    }

    #[test]
    fn decodes_png_svg_and_rejects_junk() {
        let (size, img) = decode(&png(40, 20), false).unwrap();
        assert_eq!(size, [40, 20]);
        assert_eq!(img.size, [40, 20]);
        assert_eq!(img.pixels[0], egui::Color32::from_rgb(200, 30, 30));

        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="20"><rect width="10" height="20" fill="blue"/></svg>"#;
        let (size, img) = decode(svg, true).unwrap();
        assert_eq!(size, [10, 20]);
        assert!(img.size[0] >= 10 && img.size[1] == img.size[0] * 2);
        assert_eq!(img.pixels[0], egui::Color32::from_rgb(0, 0, 255));

        assert!(decode(b"not a picture", false).is_err());
        assert!(decode(b"not a drawing", true).is_err());
    }

    #[test]
    fn big_pictures_shrink_and_small_ones_grow() {
        let (_, img) = decode(&png(3000, 1500), false).unwrap();
        assert_eq!(img.size, [1024, 512]);
        let big = ImageSide::ok("After", [3000, 1500], img);
        assert_eq!(big.display_size(), vec2(BOX_W, BOX_W / 2.0));
        let icon = ImageSide::ok("After", [16, 16], decode(&png(16, 16), false).unwrap().1);
        assert_eq!(icon.display_size(), vec2(96.0, 96.0));
        let p = ImagePreview { sides: vec![icon] };
        assert_eq!(p.rows(), 1 + (96.0 / DIFF_ROW).ceil() as usize + 1);
    }
}
