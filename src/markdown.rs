//! Markdown for pull request descriptions and comments: read with
//! `pulldown-cmark` into a few kinds of block, then shown with egui labels in
//! Headway's own colours (code blocks coloured like diffs).
//!
//! Not a full renderer: images show as links, HTML shows as its text (comments
//! dropped), footnotes as plain text. Parse once (on a worker for long bodies),
//! show every frame.

use crate::highlight::{self, Highlighted};
use crate::theme::{self, *};
use egui::{Color32, Margin, RichText, Stroke, Ui};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use std::sync::Arc;

/// A run of text with one style.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub code: bool,
    pub link: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    /// Level 1 to 6.
    Heading(u8, Vec<Span>),
    Text(Vec<Span>),
    /// A list item: its marker ("•", "3.", "☑", "☐") and first paragraph.
    Item(String, Vec<Span>),
    /// A code block: its language (if given) and text.
    Code(Option<String>, String),
    Rule,
    /// Rows of cells (the first is the header).
    Table(Vec<Vec<Vec<Span>>>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    /// List nesting (0: not in a list).
    pub indent: u8,
    /// Inside a block quote.
    pub quote: bool,
    pub kind: Kind,
}

/// Parsed markdown, ready to show.
#[derive(Clone, Debug, Default)]
pub struct Doc {
    pub blocks: Vec<Block>,
    /// Syntax colours of each code block, by block index.
    pub code: Vec<(usize, Arc<Highlighted>)>,
}

impl Doc {
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// The text without styling, a block per line (for tests and screen readers).
    #[cfg(test)]
    pub fn plain(&self) -> String {
        let spans = |s: &[Span]| s.iter().map(|s| s.text.as_str()).collect::<String>();
        self.blocks
            .iter()
            .map(|b| {
                let pad = "  ".repeat(b.indent.saturating_sub(1) as usize) + if b.quote { "> " } else { "" };
                pad + &match &b.kind {
                    Kind::Heading(l, s) => format!("{} {}", "#".repeat(*l as usize), spans(s)),
                    Kind::Text(s) => spans(s),
                    Kind::Item(m, s) => format!("{m} {}", spans(s)),
                    Kind::Code(lang, t) => format!("```{}\n{t}```", lang.as_deref().unwrap_or("")),
                    Kind::Rule => "---".into(),
                    Kind::Table(rows) => rows.iter().map(|r| r.iter().map(|c| spans(c)).collect::<Vec<_>>().join(" | ")).collect::<Vec<_>>().join("\n"),
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Read markdown (GitHub's flavour: tables, task lists, strikethrough).
pub fn parse(text: &str) -> Doc {
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS | Options::ENABLE_GFM;
    let mut p = Reader::default();
    for ev in Parser::new_ext(text, opts) {
        p.event(ev);
    }
    p.flush();
    let code = p
        .blocks
        .iter()
        .enumerate()
        .filter_map(|(i, b)| match &b.kind {
            Kind::Code(Some(lang), text) => highlight::highlight(&format!("code.{}", extension(lang)), text).map(|h| (i, Arc::new(h))),
            _ => None,
        })
        .collect();
    Doc { blocks: p.blocks, code }
}

/// A file extension for a code block's language name ("python" → "py").
fn extension(lang: &str) -> &str {
    let lang = lang.split([',', ' ', '{']).next().unwrap_or(lang);
    match lang.to_ascii_lowercase().as_str() {
        "python" | "python3" => "py",
        "rust" => "rs",
        "javascript" | "node" => "js",
        "typescript" => "ts",
        "shell" | "bash" | "zsh" | "console" | "shell-session" => "sh",
        "ruby" => "rb",
        "golang" => "go",
        "yml" => "yaml",
        "markdown" => "md",
        "csharp" | "c#" => "cs",
        "c++" | "cpp" => "cpp",
        "kotlin" => "kt",
        "haskell" => "hs",
        "elixir" => "ex",
        "perl" => "pl",
        _ => lang,
    }
}

#[derive(Default)]
struct Reader {
    blocks: Vec<Block>,
    spans: Vec<Span>,
    /// What the spans being read will become (`None`: a paragraph).
    pending: Option<Kind>,
    heading: Option<u8>,
    /// Open lists: the next number of each (`None`: bullets).
    lists: Vec<Option<u64>>,
    quote: u8,
    bold: u8,
    italic: u8,
    strike: u8,
    links: Vec<String>,
    code: Option<(Option<String>, String)>,
    table: Option<Vec<Vec<Vec<Span>>>>,
    in_html_comment: bool,
}

impl Reader {
    fn indent(&self) -> u8 {
        self.lists.len().min(8) as u8
    }

    fn push(&mut self, kind: Kind) {
        self.blocks.push(Block { indent: self.indent(), quote: self.quote > 0, kind });
    }

    /// Finish the block being read.
    fn flush(&mut self) {
        let spans = trim(std::mem::take(&mut self.spans));
        match self.pending.take() {
            Some(Kind::Item(marker, _)) => self.push(Kind::Item(marker, spans)),
            _ if spans.is_empty() => {}
            _ => match self.heading {
                Some(l) => self.push(Kind::Heading(l, spans)),
                None => self.push(Kind::Text(spans)),
            },
        }
    }

    fn text(&mut self, t: &str, code: bool) {
        if let Some(cell) = self.table.as_mut().and_then(|t| t.last_mut()).and_then(|r| r.last_mut()) {
            cell.push(Span { text: t.to_string(), code, bold: self.bold > 0, italic: self.italic > 0, strike: self.strike > 0, link: self.links.last().cloned() });
            return;
        }
        let span = Span { text: t.to_string(), bold: self.bold > 0, italic: self.italic > 0, strike: self.strike > 0, code, link: self.links.last().cloned() };
        // Join runs of the same style (fewer labels to lay out).
        match self.spans.last_mut() {
            Some(last) if !code && !last.code && Span { text: String::new(), ..last.clone() } == Span { text: String::new(), ..span.clone() } => last.text += t,
            _ => self.spans.push(span),
        }
    }

    fn html(&mut self, h: &str) {
        // Drop comments (pull request templates are full of them) and tags; keep text.
        let mut out = String::new();
        let mut rest = h;
        while !rest.is_empty() {
            if self.in_html_comment {
                match rest.find("-->") {
                    Some(i) => {
                        rest = &rest[i + 3..];
                        self.in_html_comment = false;
                    }
                    None => rest = "",
                }
            } else if let Some(r) = rest.strip_prefix("<!--") {
                rest = r;
                self.in_html_comment = true;
            } else if rest.starts_with('<') {
                let end = rest.find('>').map_or(rest.len(), |i| i + 1);
                let tag = rest[..end].to_ascii_lowercase();
                if tag.starts_with("<br") || tag.starts_with("<p") || tag.starts_with("</p") {
                    out.push('\n');
                }
                rest = &rest[end..];
            } else {
                let end = rest.find('<').unwrap_or(rest.len());
                out += &rest[..end];
                rest = &rest[end..];
            }
        }
        if !out.trim().is_empty() {
            self.text(out.trim_matches('\n'), false);
        } else if out.contains('\n') && !self.spans.is_empty() {
            self.text("\n", false);
        }
    }

    fn event(&mut self, ev: Event) {
        if let Some((_, buf)) = &mut self.code {
            match ev {
                Event::Text(t) => *buf += &t,
                Event::End(TagEnd::CodeBlock) => {
                    let (lang, text) = self.code.take().unwrap_or_default();
                    self.push(Kind::Code(lang, text));
                }
                _ => {}
            }
            return;
        }
        match ev {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {
                    // A list item's later paragraphs are text under it.
                    if !matches!(self.pending, Some(Kind::Item(..))) || !self.spans.is_empty() {
                        self.flush();
                    }
                }
                Tag::Heading { level, .. } => {
                    self.flush();
                    self.heading = Some(heading_level(level));
                }
                Tag::BlockQuote(_) => {
                    self.flush();
                    self.quote += 1;
                }
                Tag::CodeBlock(kind) => {
                    self.flush();
                    let lang = match kind {
                        CodeBlockKind::Fenced(l) if !l.trim().is_empty() => Some(l.trim().to_string()),
                        _ => None,
                    };
                    self.code = Some((lang, String::new()));
                }
                Tag::List(start) => {
                    self.flush();
                    self.lists.push(start);
                }
                Tag::Item => {
                    self.flush();
                    let marker = match self.lists.last_mut() {
                        Some(Some(n)) => {
                            *n += 1;
                            format!("{}.", *n - 1)
                        }
                        _ => if self.lists.len() > 1 { "◦" } else { "•" }.to_string(),
                    };
                    self.pending = Some(Kind::Item(marker, Vec::new()));
                }
                Tag::Emphasis => self.italic += 1,
                Tag::Strong => self.bold += 1,
                Tag::Strikethrough => self.strike += 1,
                Tag::Link { dest_url, .. } => self.links.push(dest_url.to_string()),
                Tag::Image { dest_url, .. } => {
                    self.links.push(dest_url.to_string());
                    self.text("🖼 ", false);
                }
                Tag::Table(_) => {
                    self.flush();
                    self.table = Some(Vec::new());
                }
                Tag::TableHead | Tag::TableRow => {
                    if let Some(t) = &mut self.table {
                        t.push(Vec::new());
                    }
                }
                Tag::TableCell => {
                    if let Some(r) = self.table.as_mut().and_then(|t| t.last_mut()) {
                        r.push(Vec::new());
                    }
                }
                Tag::HtmlBlock => self.flush(),
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph => {
                    if !matches!(self.pending, Some(Kind::Item(..))) {
                        self.flush();
                    }
                }
                TagEnd::Heading(_) => {
                    self.flush();
                    self.heading = None;
                }
                TagEnd::BlockQuote(_) => {
                    self.flush();
                    self.quote = self.quote.saturating_sub(1);
                }
                TagEnd::List(_) => {
                    self.flush();
                    self.lists.pop();
                }
                TagEnd::Item => self.flush(),
                TagEnd::Emphasis => self.italic = self.italic.saturating_sub(1),
                TagEnd::Strong => self.bold = self.bold.saturating_sub(1),
                TagEnd::Strikethrough => self.strike = self.strike.saturating_sub(1),
                TagEnd::Link | TagEnd::Image => {
                    self.links.pop();
                }
                TagEnd::Table => {
                    if let Some(rows) = self.table.take() {
                        self.push(Kind::Table(rows));
                    }
                }
                TagEnd::HtmlBlock => self.flush(),
                _ => {}
            },
            Event::Text(t) => self.text(&t, false),
            Event::Code(t) => self.text(&t, true),
            Event::InlineMath(t) | Event::DisplayMath(t) => self.text(&t, true),
            Event::Html(h) | Event::InlineHtml(h) => self.html(&h),
            Event::SoftBreak => self.text(" ", false),
            Event::HardBreak => self.text("\n", false),
            Event::Rule => {
                self.flush();
                self.push(Kind::Rule);
            }
            Event::TaskListMarker(done) => {
                if let Some(Kind::Item(m, _)) = &mut self.pending {
                    *m = if done { "☑" } else { "☐" }.into();
                }
            }
            Event::FootnoteReference(r) => self.text(&format!("[{r}]"), false),
        }
    }
}

fn heading_level(l: HeadingLevel) -> u8 {
    match l {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// Without spaces at the ends of the block.
fn trim(mut spans: Vec<Span>) -> Vec<Span> {
    while spans.first().is_some_and(|s| s.text.trim().is_empty() && !s.code) {
        spans.remove(0);
    }
    while spans.last().is_some_and(|s| s.text.trim().is_empty() && !s.code) {
        spans.pop();
    }
    if let Some(f) = spans.first_mut().filter(|s| !s.code) {
        f.text = f.text.trim_start().to_string();
    }
    if let Some(l) = spans.last_mut().filter(|s| !s.code) {
        l.text = l.text.trim_end().to_string();
    }
    spans
}

// ---------------------------------------------------------------------------
// Showing it

const BODY: f32 = 13.5;

/// Show a parsed document in the width available.
pub fn show(ui: &mut Ui, doc: &Doc) {
    let dark = ui.visuals().dark_mode;
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        for (i, b) in doc.blocks.iter().enumerate() {
            let pad = 18.0 * b.indent.saturating_sub(1) as f32;
            ui.horizontal(|ui| {
                ui.add_space(pad);
                if b.quote {
                    let (r, _) = ui.allocate_exact_size(egui::vec2(3.0, 16.0), egui::Sense::hover());
                    ui.painter().rect_filled(r, 1.0, BORDER);
                }
                ui.vertical(|ui| block(ui, b, doc.code.iter().find(|(j, _)| *j == i).map(|(_, h)| h.as_ref()), dark));
            });
        }
    });
}

fn block(ui: &mut Ui, b: &Block, colours: Option<&Highlighted>, dark: bool) {
    match &b.kind {
        Kind::Heading(level, spans) => {
            let size = match level {
                1 => 20.0,
                2 => 17.0,
                3 => 15.0,
                _ => BODY,
            };
            ui.add_space(if *level <= 2 { 6.0 } else { 2.0 });
            line(ui, spans, size, true, if b.quote { DIM } else { TEXT });
            if *level <= 2 {
                let w = ui.available_width();
                let (r, _) = ui.allocate_exact_size(egui::vec2(w, 1.0), egui::Sense::hover());
                ui.painter().rect_filled(r, 0.0, BORDER);
            }
        }
        Kind::Text(spans) => line(ui, spans, BODY, false, if b.quote { DIM } else { TEXT }),
        Kind::Item(marker, spans) => {
            ui.horizontal_top(|ui| {
                let done = marker == "☑";
                // Centred on the first line, which is as tall as a widget (text centred in it).
                let row = ui.spacing().interact_size.y.max(ui.text_style_height(&egui::TextStyle::Body));
                if marker == "•" || marker == "◦" {
                    // Drawn: the font's dot is small and sits high.
                    let (r, _) = ui.allocate_exact_size(egui::vec2(10.0, row), egui::Sense::hover());
                    if marker == "•" {
                        ui.painter().circle_filled(r.center(), 2.5, DIM);
                    } else {
                        ui.painter().circle_stroke(r.center(), 2.5, Stroke::new(1.0, DIM));
                    }
                } else {
                    let text = RichText::new(marker).font(theme::prop(BODY)).color(if done { ACCENT } else { DIM });
                    ui.allocate_ui_with_layout(egui::vec2(0.0, row), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.add(egui::Label::new(text).selectable(false));
                    });
                }
                ui.vertical(|ui| line(ui, spans, BODY, false, if b.quote { DIM } else { TEXT }));
            });
        }
        Kind::Code(_, text) => code_block(ui, text, colours, dark),
        Kind::Rule => {
            let w = ui.available_width();
            let (r, _) = ui.allocate_exact_size(egui::vec2(w, 9.0), egui::Sense::hover());
            ui.painter().hline(r.x_range(), r.center().y, Stroke::new(1.0, BORDER));
        }
        Kind::Table(rows) => {
            // Cells keep to one line (wrapping squeezes narrow columns down to
            // a letter or two) and a wide table scrolls sideways, as on GitHub.
            let id = ui.next_auto_id();
            egui::ScrollArea::horizontal().id_salt(id).auto_shrink([false, true]).show(ui, |ui| {
                egui::Frame::new().stroke(Stroke::new(1.0, BORDER)).corner_radius(4.0).inner_margin(Margin::same(6)).show(ui, |ui| {
                    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                    egui::Grid::new(id.with("grid")).striped(true).spacing(egui::vec2(16.0, 4.0)).show(ui, |ui| {
                        for (ri, row) in rows.iter().enumerate() {
                            for cell in row {
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 0.0;
                                    for s in cell {
                                        span(ui, s, BODY, ri == 0, TEXT);
                                    }
                                });
                            }
                            ui.end_row();
                        }
                    });
                });
            });
        }
    }
}

/// Spans flowing as one wrapped paragraph.
fn line(ui: &mut Ui, spans: &[Span], size: f32, bold: bool, color: Color32) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for s in spans {
            for (k, part) in s.text.split('\n').enumerate() {
                if k > 0 {
                    ui.end_row();
                }
                if !part.is_empty() {
                    span(ui, &Span { text: part.to_string(), ..s.clone() }, size, bold, color);
                }
            }
        }
    });
}

fn span(ui: &mut Ui, s: &Span, size: f32, bold: bool, color: Color32) {
    let mut t = if s.code { RichText::new(&s.text).font(theme::mono(size - 1.0)).background_color(RAISED) } else { RichText::new(&s.text).font(theme::prop(size)) };
    if s.bold || bold {
        t = t.strong();
    }
    if s.italic {
        t = t.italics();
    }
    if s.strike {
        t = t.strikethrough();
    }
    match &s.link {
        Some(url) => {
            ui.hyperlink_to(t.color(BLUE), url).on_hover_text(url);
        }
        None => {
            ui.add(egui::Label::new(t.color(if s.bold || bold { TEXT } else { color })));
        }
    }
}

fn code_block(ui: &mut Ui, text: &str, colours: Option<&Highlighted>, dark: bool) {
    let font = theme::mono(12.5);
    let mut job = egui::text::LayoutJob::default();
    let plain = theme::syntax(highlight::Tok::Plain, dark);
    let text = text.strip_suffix('\n').unwrap_or(text);
    for (n, l) in text.split('\n').enumerate() {
        if n > 0 {
            job.append("\n", 0.0, egui::TextFormat::simple(font.clone(), plain));
        }
        match colours.and_then(|h| h.line(n as u32 + 1, l)) {
            Some(parts) => {
                let mut at = 0usize;
                for p in parts {
                    let (s, e) = (p.start as usize, p.end as usize);
                    if s > at {
                        job.append(&l[at..s], 0.0, egui::TextFormat::simple(font.clone(), plain));
                    }
                    job.append(&l[s..e], 0.0, egui::TextFormat::simple(font.clone(), theme::syntax(p.tok, dark)));
                    at = e;
                }
                if at < l.len() {
                    job.append(&l[at..], 0.0, egui::TextFormat::simple(font.clone(), plain));
                }
            }
            None => job.append(l, 0.0, egui::TextFormat::simple(font.clone(), plain)),
        }
    }
    egui::Frame::new().fill(if dark { BG } else { Color32::from_rgb(246, 248, 250) }).stroke(Stroke::new(1.0, BORDER)).corner_radius(4.0).inner_margin(Margin::same(8)).show(ui, |ui| {
        egui::ScrollArea::horizontal().id_salt(ui.next_auto_id()).auto_shrink([false, true]).show(ui, |ui| {
            ui.add(egui::Label::new(job).extend());
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_github_description_reads_as_blocks() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!("forge/fixtures/gh-pr-details.json")).unwrap();
        let body = fixture["data"]["repository"]["pullRequest"]["body"].as_str().unwrap();
        let doc = parse(body);
        assert_eq!(
            doc.plain(),
            "## Why\n\
Cart totals were off by a cent when tax was applied per line. See the issue.\n\
## What changed\n\
• Round once, at the end\n\
• Keep Decimal all the way through\n\
\x20 ◦ including discounts\n\
1. first\n\
2. second\n\
```python\ntotal = sum(lines).quantize(CENT)\n```\n\
> Tested with the receipts from #11.\n\
---\n\
☑ tests\n\
☐ docs"
        );
        // Styles and links survive.
        let Kind::Text(p) = &doc.blocks[1].kind else { panic!("a paragraph") };
        assert!(p.iter().any(|s| s.bold && s.text == "tax"));
        assert!(p.iter().any(|s| s.link.as_deref() == Some("https://github.com/acme/shop/issues/11") && s.text == "the issue"));
        let Kind::Item(_, keep) = &doc.blocks[4].kind else { panic!("an item") };
        assert!(keep.iter().any(|s| s.code && s.text == "Decimal"));
        assert_eq!((doc.blocks[5].indent, doc.blocks[4].indent), (2, 1), "nested list");
        assert!(doc.blocks[9].quote);
        // The python block is coloured.
        assert_eq!(doc.code.iter().map(|c| c.0).collect::<Vec<_>>(), [8]);
    }

    #[test]
    fn a_gitlab_description_and_comments_read_as_blocks() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!("forge/fixtures/glab-mr-details.json")).unwrap();
        let doc = parse(fixture["data"]["project"]["mergeRequest"]["description"].as_str().unwrap());
        assert_eq!(doc.plain(), "Fixes rounding.\n```\nindented code\n```\n• one\n• two");
        assert!(doc.code.is_empty(), "no language, no colours");

        let conv: serde_json::Value = serde_json::from_str(include_str!("forge/fixtures/gh-pr-conversation.json")).unwrap();
        let comment = conv["data"]["repository"]["pullRequest"]["reviews"]["nodes"][0]["comments"]["nodes"][0]["body"].as_str().unwrap();
        assert_eq!(parse(comment).plain(), "This rounds before the discount:\n```python\nprice.quantize(CENT)\n```");
    }

    #[test]
    fn templates_html_tables_and_loose_lists() {
        let md = "<!-- Describe your change -->\nHello<br>world\n\n| a | b |\n|---|---|\n| 1 | **2** |\n\n- one\n\n  more about one\n- ~~two~~\n\n![shot](https://x/y.png)";
        let doc = parse(md);
        assert_eq!(doc.plain(), "Hello\nworld\na | b\n1 | 2\n• one\nmore about one\n• two\n🖼 shot");
        assert!(matches!(&doc.blocks[4].kind, Kind::Item(_, s) if s[0].strike));
        assert!(matches!(&doc.blocks[5].kind, Kind::Text(s) if s[0].link.as_deref() == Some("https://x/y.png")));
        assert!(parse("").is_empty());
        assert!(parse("<!--\nonly a\ncomment\n-->").is_empty());
    }
}
