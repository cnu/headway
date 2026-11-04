//! Colours, spacing and small formatting helpers.

use egui::{Color32, CornerRadius, FontFamily, FontId, Stroke, TextStyle, Visuals};

pub const BG: Color32 = Color32::from_rgb(27, 30, 35);
pub const PANEL: Color32 = Color32::from_rgb(33, 37, 43);
pub const SIDEBAR: Color32 = Color32::from_rgb(29, 32, 38);
pub const RAISED: Color32 = Color32::from_rgb(42, 47, 55);
pub const BORDER: Color32 = Color32::from_rgb(50, 56, 65);
pub const TEXT: Color32 = Color32::from_rgb(214, 219, 226);
pub const DIM: Color32 = Color32::from_rgb(138, 147, 160);
pub const FAINT: Color32 = Color32::from_rgb(95, 103, 115);
pub const ACCENT: Color32 = Color32::from_rgb(78, 196, 141);
pub const ACCENT_DIM: Color32 = Color32::from_rgb(38, 82, 64);
pub const SELECT: Color32 = Color32::from_rgb(44, 62, 78);
pub const HOVER: Color32 = Color32::from_rgb(38, 43, 51);
pub const WARN: Color32 = Color32::from_rgb(229, 181, 103);
pub const DANGER: Color32 = Color32::from_rgb(232, 107, 107);
pub const BLUE: Color32 = Color32::from_rgb(102, 163, 238);
pub const PURPLE: Color32 = Color32::from_rgb(178, 132, 226);

pub const ADD_BG: Color32 = Color32::from_rgb(30, 58, 44);
pub const ADD_FG: Color32 = Color32::from_rgb(140, 220, 170);
pub const DEL_BG: Color32 = Color32::from_rgb(66, 34, 38);
pub const DEL_FG: Color32 = Color32::from_rgb(240, 150, 150);
pub const HUNK_BG: Color32 = Color32::from_rgb(36, 44, 58);
/// Code that isn't a changed line (and uncoloured parts of highlighted lines).
pub const CODE: Color32 = Color32::from_rgb(196, 202, 210);
pub const CODE_LIGHT: Color32 = Color32::from_rgb(36, 41, 47);

/// Behind the words that changed within a removed (`added == false`) or added line.
pub fn emphasis(added: bool, dark: bool) -> Color32 {
    match (added, dark) {
        (true, true) => Color32::from_rgb(44, 112, 74),
        (false, true) => Color32::from_rgb(124, 50, 58),
        (true, false) => Color32::from_rgb(168, 234, 184),
        (false, false) => Color32::from_rgb(255, 186, 186),
    }
}

/// The colour of a kind of token in code, for a dark or a light background.
pub fn syntax(tok: crate::highlight::Tok, dark: bool) -> Color32 {
    use crate::highlight::Tok::*;
    let rgb = |d: (u8, u8, u8), l: (u8, u8, u8)| {
        let (r, g, b) = if dark { d } else { l };
        Color32::from_rgb(r, g, b)
    };
    match tok {
        Plain => if dark { CODE } else { CODE_LIGHT },
        Comment => rgb((112, 121, 135), (106, 115, 125)),
        String => rgb((158, 206, 128), (10, 110, 50)),
        Escape => rgb((86, 196, 204), (0, 115, 125)),
        Number | Constant => rgb((229, 170, 103), (170, 85, 0)),
        Keyword => rgb((198, 146, 234), (135, 60, 180)),
        Type => rgb((229, 200, 110), (140, 95, 0)),
        Function => rgb((110, 170, 240), (25, 90, 190)),
        Tag => rgb((232, 107, 140), (185, 30, 80)),
        Attribute => rgb((229, 170, 103), (160, 85, 10)),
        Macro => rgb((86, 196, 204), (0, 115, 125)),
    }
}

pub const LANES: [Color32; 8] = [
    Color32::from_rgb(78, 196, 141),
    Color32::from_rgb(102, 163, 238),
    Color32::from_rgb(232, 137, 90),
    Color32::from_rgb(178, 132, 226),
    Color32::from_rgb(229, 192, 103),
    Color32::from_rgb(86, 196, 204),
    Color32::from_rgb(232, 107, 140),
    Color32::from_rgb(160, 170, 185),
];

pub fn lane(i: usize) -> Color32 {
    LANES[i % LANES.len()]
}

/// How recent a change is, from `t` = 0 (the oldest shown) to 1 (the newest):
/// newer is a stronger, warmer colour.
pub fn age(t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let old = Color32::from_rgb(58, 66, 80);
    let new = Color32::from_rgb(240, 146, 64);
    let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
    Color32::from_rgb(mix(old.r(), new.r()), mix(old.g(), new.g()), mix(old.b(), new.b()))
}

/// Where `time` sits between `oldest` and `newest` for [`age`]. Measured in days on a
/// log scale, so last week's changes stand apart from last month's even in an old file.
pub fn age_of(time: i64, oldest: i64, newest: i64) -> f32 {
    if newest <= oldest {
        return 1.0;
    }
    let days = |s: i64| (s.max(0) as f32 / 86_400.0 + 1.0).ln();
    1.0 - days(newest - time) / days(newest - oldest)
}

pub fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

pub fn prop(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

pub fn apply(ctx: &egui::Context) {
    let mut v = Visuals::dark();
    v.panel_fill = PANEL;
    v.window_fill = RAISED;
    v.window_stroke = Stroke::new(1.0, BORDER);
    v.extreme_bg_color = Color32::from_rgb(22, 24, 28);
    v.faint_bg_color = SIDEBAR;
    v.code_bg_color = RAISED;
    v.hyperlink_color = BLUE;
    v.override_text_color = Some(TEXT);
    v.selection.bg_fill = Color32::from_rgb(45, 92, 120);
    v.selection.stroke = Stroke::new(1.0, TEXT);
    v.window_corner_radius = CornerRadius::same(8);
    v.menu_corner_radius = CornerRadius::same(6);

    let w = &mut v.widgets;
    w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    w.inactive.weak_bg_fill = RAISED;
    w.inactive.bg_fill = RAISED;
    w.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    w.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    w.hovered.weak_bg_fill = Color32::from_rgb(52, 59, 69);
    w.hovered.bg_fill = Color32::from_rgb(52, 59, 69);
    w.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(70, 78, 90));
    w.active.weak_bg_fill = Color32::from_rgb(60, 68, 80);
    for s in [&mut w.noninteractive, &mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
        s.corner_radius = CornerRadius::same(5);
    }

    ctx.set_visuals(v);
    ctx.global_style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(8.0, 5.0);
        s.spacing.button_padding = egui::vec2(9.0, 4.0);
        s.spacing.interact_size.y = 24.0;
        s.text_styles.insert(TextStyle::Body, prop(13.5));
        s.text_styles.insert(TextStyle::Button, prop(13.5));
        s.text_styles.insert(TextStyle::Small, prop(11.5));
        s.text_styles.insert(TextStyle::Heading, prop(19.0));
        s.text_styles.insert(TextStyle::Monospace, mono(12.5));
    });
}

// ---------------------------------------------------------------------------
// Time formatting (no external date library needed).

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn relative(ts: i64) -> String {
    let d = (now() - ts).max(0);
    match d {
        0..=59 => "just now".into(),
        60..=3599 => plural(d / 60, "minute"),
        3600..=86_399 => plural(d / 3600, "hour"),
        86_400..=604_799 => plural(d / 86_400, "day"),
        604_800..=2_591_999 => plural(d / 604_800, "week"),
        _ => date(ts),
    }
}

fn plural(n: i64, unit: &str) -> String {
    if n == 1 { format!("1 {unit} ago") } else { format!("{n} {unit}s ago") }
}

/// YYYY-MM-DD in UTC.
pub fn date(ts: i64) -> String {
    let (y, m, d) = civil(ts.div_euclid(86_400));
    format!("{y:04}-{m:02}-{d:02}")
}

pub fn date_time(ts: i64) -> String {
    let secs = ts.rem_euclid(86_400);
    format!("{} {:02}:{:02} UTC", date(ts), secs / 3600, (secs % 3600) / 60)
}

fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

pub fn initials(name: &str) -> String {
    name.split_whitespace().filter_map(|w| w.chars().next()).take(2).collect::<String>().to_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_are_formatted_in_utc() {
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(date(951_782_400), "2000-02-29"); // leap day
        assert_eq!(date(1_798_761_599), "2026-12-31"); // last second of the year
        assert_eq!(date(1_798_761_600), "2027-01-01");
        assert_eq!(date(-86_400), "1969-12-31");
        assert_eq!(date_time(951_782_400 + 13 * 3600 + 5 * 60), "2000-02-29 13:05 UTC");
    }

    #[test]
    fn relative_times_read_naturally() {
        let n = now();
        assert_eq!(relative(n), "just now");
        assert_eq!(relative(n + 500), "just now"); // clock skew: never "in the future"
        assert_eq!(relative(n - 60), "1 minute ago");
        assert_eq!(relative(n - 5 * 60), "5 minutes ago");
        assert_eq!(relative(n - 3 * 3600), "3 hours ago");
        assert_eq!(relative(n - 86_400), "1 day ago");
        assert_eq!(relative(n - 2 * 604_800), "2 weeks ago");
        // Older than a month shows the date.
        assert_eq!(relative(n - 40 * 86_400), date(n - 40 * 86_400));
    }

    #[test]
    fn initials_take_the_first_two_words() {
        assert_eq!(initials("Sam Okafor"), "SO");
        assert_eq!(initials("jo"), "J");
        assert_eq!(initials("Ana María de la Cruz"), "AM");
        assert_eq!(initials(""), "");
    }

    #[test]
    fn lane_colours_repeat() {
        assert_eq!(lane(0), lane(LANES.len()));
        assert_ne!(lane(0), lane(1));
    }
}
