//! Sample projects, so a first launch has something real to explore.
//!
//! Only development builds include them. Release builds start with an empty
//! workspace and get the stand-ins at the bottom of this file instead.

pub use imp::*;

#[cfg(debug_assertions)]
mod imp {
    use crate::workspace::Workspace;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    fn samples_dir() -> PathBuf {
        dirs::data_dir().unwrap_or_else(std::env::temp_dir).join("headway").join("samples")
    }

    struct Builder<'a> {
        dir: &'a Path,
        clock: i64,
    }

    impl Builder<'_> {
        fn git(&self, args: &[&str]) {
            let date = format!("{} +0000", self.clock);
            let _ = Command::new("git")
                .arg("-C")
                .arg(self.dir)
                .args(args)
                .env("GIT_AUTHOR_DATE", &date)
                .env("GIT_COMMITTER_DATE", &date)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output();
        }

        /// The commit id `rev` names.
        fn rev(&self, rev: &str) -> String {
            Command::new("git")
                .arg("-C")
                .arg(self.dir)
                .args(["rev-parse", rev])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_default()
        }

        fn write(&self, file: &str, text: &str) {
            let p = self.dir.join(file);
            if let Some(parent) = p.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(p, text);
        }

        fn write_bytes(&self, file: &str, bytes: &[u8]) {
            let p = self.dir.join(file);
            if let Some(parent) = p.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(p, bytes);
        }

        fn commit(&mut self, who: (&str, &str), msg: &str, hours_later: i64) {
            self.clock += hours_later * 3600;
            self.git(&["add", "-A"]);
            let name = format!("user.name={}", who.0);
            let email = format!("user.email={}", who.1);
            self.git(&["-c", &name, "-c", &email, "commit", "-q", "-m", msg]);
        }
    }

    const ALEX: (&str, &str) = ("Alex Rivera", "alex@example.com");
    const SAM: (&str, &str) = ("Sam Okafor", "sam@example.com");
    const JO: (&str, &str) = ("Jo Lindqvist", "jo@example.com");

    fn now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    }

    /// Fills a fresh repository with a sample project.
    type Make = fn(&mut Builder);

    const SAMPLES: [(&str, Make); 5] = [
        ("web-shop", build_web_shop),
        ("api-server", build_api),
        ("design-tokens", build_tokens),
        ("price-rules", build_price_rules),
        ("diff-showcase", build_diff_showcase),
    ];

    pub fn sample_workspace() -> Workspace {
        let root = samples_dir();
        let mut ws = Workspace::new("Sample Projects");
        for (name, _) in SAMPLES {
            ws.repos.push(root.join(name));
        }
        ws.open_tabs = vec![ws.repos[0].clone()];
        ws.active_tab = Some(0);
        ws
    }

    /// True when this workspace points at sample repositories that don't exist yet.
    pub fn samples_missing(ws: &Workspace) -> bool {
        let root = samples_dir();
        ws.repos.iter().any(|r| r.starts_with(&root) && !r.join(".git").exists())
    }

    /// Create the sample repositories that are missing. Slow: call from a worker thread.
    pub fn build_samples() {
        let root = samples_dir();
        for (name, make) in SAMPLES {
            let dir = root.join(name);
            if !dir.join(".git").exists() {
                build_one(&dir, make);
            }
        }
    }

    fn build_one(dir: &Path, make: fn(&mut Builder)) {
        let _ = std::fs::create_dir_all(dir);
        let mut b = Builder { dir, clock: now() - 21 * 24 * 3600 };
        b.git(&["init", "-q", "-b", "main"]);
        make(&mut b);
    }

    /// Which sample project this folder is, if it's one of Headway's samples.
    pub fn sample_name(path: &Path) -> Option<&'static str> {
        let root = samples_dir();
        SAMPLES.iter().map(|(n, _)| *n).find(|n| path == root.join(n))
    }

    /// Delete a sample project (history, branches and any changes in it) and build it
    /// again from scratch, exactly as on first launch.
    pub fn reset_sample(path: &Path) -> Result<(), String> {
        let name = sample_name(path).ok_or_else(|| format!("{} isn't one of Headway's sample projects", path.display()))?;
        let (_, make) = SAMPLES.iter().find(|(n, _)| *n == name).copied().ok_or("unknown sample")?;
        if path.exists() {
            std::fs::remove_dir_all(path).map_err(|e| format!("Couldn't remove the old sample: {e}"))?;
        }
        build_one(path, make);
        if path.join(".git").exists() { Ok(()) } else { Err("Couldn't build the sample again".into()) }
    }

    fn build_web_shop(b: &mut Builder) {
        b.write("README.md", "# Web Shop\n\nA small storefront.\n");
        b.write("src/index.html", "<!doctype html>\n<html>\n  <body>\n    <h1>Shop</h1>\n  </body>\n</html>\n");
        b.commit(ALEX, "Initial storefront skeleton", 0);
        b.write("src/style.css", "body {\n  font-family: sans-serif;\n  margin: 0;\n}\n");
        b.commit(ALEX, "Add base stylesheet", 20);
        b.write("src/cart.js", "export function total(items) {\n  return items.reduce((s, i) => s + i.price, 0);\n}\n");
        b.commit(SAM, "Add cart total calculation", 26);
        b.git(&["checkout", "-q", "-b", "feature/checkout"]);
        b.write("src/checkout.js", "import { total } from './cart.js';\n\nexport function checkout(items) {\n  const sum = total(items);\n  return { sum, status: 'pending' };\n}\n");
        b.commit(SAM, "Start checkout flow", 5);
        b.write("src/checkout.js", "import { total } from './cart.js';\n\nexport function checkout(items, coupon) {\n  let sum = total(items);\n  if (coupon) sum *= 0.9;\n  return { sum, status: 'pending' };\n}\n");
        b.commit(SAM, "Support coupon codes at checkout", 8);
        b.git(&["checkout", "-q", "main"]);
        b.write("src/style.css", "body {\n  font-family: system-ui, sans-serif;\n  margin: 0;\n  color: #222;\n}\n\nh1 {\n  font-size: 2rem;\n}\n");
        b.commit(JO, "Refine typography", 12);
        b.git(&["-c", "user.name=Alex Rivera", "-c", "user.email=alex@example.com", "merge", "-q", "--no-ff", "feature/checkout", "-m", "Merge branch 'feature/checkout'"]);
        b.clock += 3600 * 30;
        b.git(&["tag", "v1.0.0"]);
        b.write("src/index.html", "<!doctype html>\n<html>\n  <head>\n    <link rel=\"stylesheet\" href=\"style.css\">\n  </head>\n  <body>\n    <h1>Shop</h1>\n    <div id=\"cart\"></div>\n  </body>\n</html>\n");
        b.commit(JO, "Link stylesheet and add cart container", 2);
        b.git(&["checkout", "-q", "-b", "feature/search"]);
        b.write("src/search.js", "export function search(items, q) {\n  q = q.toLowerCase();\n  return items.filter(i => i.name.toLowerCase().includes(q));\n}\n");
        b.commit(ALEX, "Add product search", 40);
        b.write("src/search.js", "export function search(items, q) {\n  q = q.trim().toLowerCase();\n  if (!q) return items;\n  return items.filter(i => i.name.toLowerCase().includes(q));\n}\n");
        b.commit(ALEX, "Ignore empty search queries", 6);
        b.git(&["checkout", "-q", "main"]);
        b.write("README.md", "# Web Shop\n\nA small storefront.\n\n## Running\n\nOpen `src/index.html` in a browser.\n");
        b.commit(SAM, "Document how to run the shop", 30);
        b.write("src/cart.js", "export function total(items) {\n  return items.reduce((s, i) => s + i.price * (i.qty ?? 1), 0);\n}\n\nexport function count(items) {\n  return items.length;\n}\n");
        b.commit(JO, "Count quantities in cart total", 20);
        // A commit thrown away with a hard reset: only the reflog still finds it.
        b.write("src/banner.js", "export const banner = 'Free shipping on orders over $50';\n");
        b.commit(ALEX, "Try a free shipping banner", 3);
        b.clock += 3600;
        b.git(&["reset", "-q", "--hard", "HEAD~1"]);
        // Uncommitted work so the "changes" view has content.
        b.write("src/cart.js", "export function total(items) {\n  return items.reduce((s, i) => s + i.price * (i.qty ?? 1), 0);\n}\n\nexport function count(items) {\n  return items.reduce((n, i) => n + (i.qty ?? 1), 0);\n}\n\nexport function isEmpty(items) {\n  return items.length === 0;\n}\n");
        b.write("src/wishlist.js", "export const wishlist = [];\n");
        b.write("src/style.css", "body {\n  font-family: system-ui, sans-serif;\n  margin: 0;\n  color: #1d1d1f;\n}\n\nh1 {\n  font-size: 2rem;\n}\n");
        b.git(&["add", "src/style.css"]);
    }

    /// The API server, written by several people over time, with `indent` spaces per level.
    fn api_server(indent: usize, orders: bool, host: &str) -> String {
        let i = |n: usize| " ".repeat(indent * n);
        let mut s = String::from("from http.server import HTTPServer, BaseHTTPRequestHandler\nimport json\n\nITEMS = [{\"id\": 1, \"name\": \"Mug\"}]\n");
        if orders {
            s += "ORDERS = []\n";
        }
        s += &format!("\n\nclass Handler(BaseHTTPRequestHandler):\n{}def do_GET(self):\n", i(1));
        s += &format!("{}if self.path == \"/health\":\n{}self.reply(200, {{\"ok\": True}})\n", i(2), i(3));
        s += &format!("{}elif self.path == \"/items\":\n{}self.reply(200, ITEMS)\n", i(2), i(3));
        if orders {
            s += &format!("{}elif self.path == \"/orders\":\n{}self.reply(200, ORDERS)\n", i(2), i(3));
        }
        s += &format!("{}else:\n{}self.reply(404, {{\"error\": \"not found\"}})\n\n", i(2), i(3));
        s += &format!("{}def reply(self, code, body):\n{}data = json.dumps(body).encode()\n{}self.send_response(code)\n", i(1), i(2), i(2));
        s += &format!("{}self.send_header(\"Content-Type\", \"application/json\")\n{}self.end_headers()\n{}self.wfile.write(data)\n\n\n", i(2), i(2), i(2));
        s += &format!("app = HTTPServer((\"{host}\", 8000), Handler)\n");
        s
    }

    /// Blame and file history: a server file several people changed, moved into a
    /// package, then reformatted (a commit `.git-blame-ignore-revs` tells blame to skip).
    fn build_api(b: &mut Builder) {
        b.write("main.py", "from server import app\n\nif __name__ == '__main__':\n    app.serve_forever()\n");
        b.commit(SAM, "Create API entry point", 0);
        b.write("server.py", "app = None\n");
        b.commit(SAM, "Add server module", 50);
        b.write("routes.py", "ROUTES = ['/health', '/items']\n");
        b.commit(ALEX, "Add health and items routes", 70);
        b.git(&["checkout", "-q", "-b", "fix/timeouts"]);
        b.write("server.py", "app = None\nTIMEOUT = 30\n");
        b.commit(JO, "Raise request timeout to 30 seconds", 48);
        b.git(&["checkout", "-q", "main"]);
        b.write("routes.py", "ROUTES = ['/health', '/items', '/orders']\n");
        b.commit(ALEX, "Add orders route", 60);
        b.write("server.py", &api_server(2, false, "127.0.0.1"));
        b.commit(ALEX, "Serve items over HTTP", 30);
        b.write("server.py", &api_server(2, true, "127.0.0.1"));
        b.commit(JO, "Answer the orders route", 50);
        b.write("api/__init__.py", "");
        b.git(&["mv", "server.py", "api/server.py"]);
        b.write("main.py", "from api.server import app\n\nif __name__ == '__main__':\n    app.serve_forever()\n");
        b.commit(SAM, "Move the server into an api package", 40);
        b.write("api/server.py", &api_server(4, true, "127.0.0.1"));
        b.commit(ALEX, "Reformat with four-space indents", 20);
        let reformat = b.rev("HEAD");
        b.write(".git-blame-ignore-revs", &format!("# Formatting only: blame looks past these.\n{reformat}\n"));
        b.commit(ALEX, "Hide the reformat from blame", 1);
        b.write("api/server.py", &api_server(4, true, "0.0.0.0"));
        b.commit(JO, "Listen on every interface", 30);
        // Uncommitted work, so blame of the working copy has something new.
        b.write("api/server.py", &(api_server(4, true, "0.0.0.0") + "DEBUG = False\n"));
    }

    fn build_tokens(b: &mut Builder) {
        b.write("tokens.json", "{\n  \"color\": { \"primary\": \"#2f6\" }\n}\n");
        b.commit(JO, "Add primary colour token", 0);
        b.write("tokens.json", "{\n  \"color\": { \"primary\": \"#2f6\", \"accent\": \"#f62\" },\n  \"space\": [4, 8, 16]\n}\n");
        b.commit(JO, "Add accent colour and spacing scale", 90);
        b.git(&["tag", "v0.2.0"]);
        b.write("CHANGELOG.md", "## 0.2.0\n- Accent colour\n- Spacing scale\n");
        b.commit(SAM, "Start a changelog", 100);
    }

    /// A merge stopped on conflicts: a text file with three conflicting regions, a
    /// file one side deleted while the other changed it, and a binary file.
    fn build_price_rules(b: &mut Builder) {
        let pricing = |tax: &str, round: &str, label: &str| {
            format!(
                "export const CURRENCY = 'EUR';\n\n{tax}\n\nexport function net(price) {{\n  return price;\n}}\n\nexport function gross(price) {{\n  const total = price * (1 + TAX_RATE);\n{round}\n}}\n\nexport function describe(item) {{\n  const price = gross(item.price);\n  const name = item.name;\n  const sku = item.sku;\n{label}\n}}\n"
            )
        };
        b.write("README.md", "# Price rules\n\nHow the shop works out prices.\n");
        b.write("src/pricing.js", &pricing("export const TAX_RATE = 0.2;", "  return total;", "  return `${name}: ${price}`;"));
        b.write("src/legacy-tax.js", "// Old tax tables, kept for invoices before 2024.\nexport const TABLE = { standard: 0.19 };\n");
        b.write("assets/badge.png", "\u{0}PNG badge v1");
        b.commit(SAM, "Start the price rules", 0);
        b.git(&["checkout", "-q", "-b", "feature/discounts"]);
        b.write("src/pricing.js", &pricing("export const TAX_RATE = 0.2;\nexport const DISCOUNT = 0.1;", "  return Math.round(total * (1 - DISCOUNT) * 100) / 100;", "  return `${name} (${sku}): ${price} after discount`;"));
        b.commit(ALEX, "Apply a 10% discount to every price", 30);
        b.git(&["rm", "-q", "src/legacy-tax.js"]);
        b.write("assets/badge.png", "\u{0}PNG badge with discount ribbon");
        b.commit(ALEX, "Drop the legacy tax table and add a sale badge", 4);
        b.git(&["checkout", "-q", "main"]);
        b.write("src/pricing.js", &pricing("export const TAX_RATE = 0.21;", "  return Math.round(total * 100) / 100;", "  return `${name}: ${price} incl. VAT`;"));
        b.commit(JO, "Raise VAT to 21% and round to cents", 20);
        b.write("src/legacy-tax.js", "// Old tax tables, kept for invoices before 2024.\nexport const TABLE = { standard: 0.19, reduced: 0.07 };\n");
        b.write("assets/badge.png", "\u{0}PNG badge v2 (new colours)");
        b.write("README.md", "# Price rules\n\nHow the shop works out prices, including VAT.\n");
        b.commit(SAM, "Add the reduced rate and refresh the badge", 6);
        // Stops with conflicts, so there's something to resolve.
        b.git(&["-c", "user.name=Sam Okafor", "-c", "user.email=sam@example.com", "merge", "feature/discounts"]);
    }

    /// A square picture of one colour with a white band across it, as a PNG.
    fn badge_png(size: u32, rgb: [u8; 3], band: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_fn(size, size, |_, y| {
            if y / 4 == band { image::Rgba([255, 255, 255, 255]) } else { image::Rgba([rgb[0], rgb[1], rgb[2], 255]) }
        });
        let mut out = std::io::Cursor::new(Vec::new());
        let _ = img.write_to(&mut out, image::ImageFormat::Png);
        out.into_inner()
    }

    /// Bytes that look like a binary model file.
    fn model_bin(len: usize, seed: u32) -> Vec<u8> {
        (0..len).map(|i| ((i as u32).wrapping_mul(2_654_435_761).wrapping_add(seed) >> 24) as u8).collect()
    }

    fn cart_rs(rate: &str, count: &str, banner: &str, extra: &str, indent: &str) -> String {
        format!(
            "//! The shopping cart.\n\n/* Prices are in cents.\n   Totals never go below zero. */\npub const TAX_RATE: f64 = {rate};\n\npub const BANNER: &str = \"Welcome back!\n{banner}\";\n\npub struct Line {{\n    pub price: u64,\n    pub {count}: u32,\n}}\n\npub fn total(lines: &[Line]) -> u64 {{\n{indent}let mut sum = 0;\n    for l in lines {{\n        sum += l.price * l.{count} as u64;\n    }}\n{extra}    (sum as f64 * (1.0 + TAX_RATE)) as u64\n}}\n"
        )
    }

    fn rows_csv(version: u32) -> String {
        std::iter::once("sku,price,stock\n".to_string()).chain((1..=1200).map(|i| format!("SKU-{i:04},{},{}\n", 100 + i * version, (i * 7 + version) % 50))).collect()
    }

    /// Something of every kind for the diff viewer: highlighted code with a
    /// multi-line string and comment, renamed words, a re-indented line, a
    /// changed picture, a binary file that grew, and a change too big to show
    /// at once (in the latest commit), plus uncommitted edits to stage.
    fn build_diff_showcase(b: &mut Builder) {
        b.write("README.md", "# Diff showcase\n\nA little of everything the diff viewer shows.\n");
        b.write("src/cart.rs", &cart_rs("0.2", "count", "Free delivery over 50 EUR.", "", "    "));
        b.write_bytes("assets/badge.png", &badge_png(48, [220, 60, 70], 5));
        b.write_bytes("data/model.bin", &model_bin(3 * 1024, 1));
        b.write("data/rows.csv", &rows_csv(1));
        b.commit(JO, "Start the cart", 0);
        b.write_bytes("assets/badge.png", &badge_png(64, [60, 110, 220], 9));
        b.write_bytes("data/model.bin", &model_bin(180 * 1024, 2));
        b.write("data/rows.csv", &rows_csv(2));
        b.commit(SAM, "Refresh the badge, retrain the model and reprice every row", 30);
        // Uncommitted: edits to stage line by line, side by side or not.
        b.write(
            "src/cart.rs",
            &cart_rs("0.21", "quantity", "Free delivery over 40 EUR.", "    // Never charge less than nothing.\n    let sum = sum.max(0);\n", "\t"),
        );
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn only_sample_repositories_count_as_missing_samples() {
            let mut ws = Workspace::new("Mine");
            ws.repos = vec![std::env::temp_dir().join("definitely-not-a-sample")];
            assert!(!samples_missing(&ws));
            ws.repos.push(samples_dir().join("headway-test-no-such-sample"));
            assert!(samples_missing(&ws));
        }

        #[test]
        fn only_headway_samples_can_be_reset() {
            assert_eq!(sample_name(&samples_dir().join("web-shop")), Some("web-shop"));
            assert_eq!(sample_name(&samples_dir().join("something-else")), None);
            assert_eq!(sample_name(Path::new("/home/me/web-shop")), None);
            assert!(reset_sample(Path::new("/home/me/web-shop")).unwrap_err().contains("isn't one of Headway's sample projects"));
        }

        #[test]
        fn sample_workspace_opens_its_first_project() {
            let ws = sample_workspace();
            assert_eq!(ws.repos.len(), SAMPLES.len());
            assert_eq!(ws.open_tabs, [ws.repos[0].clone()]);
            assert_eq!(ws.active_tab, Some(0));
        }

        #[test]
        fn web_shop_has_a_commit_only_the_reflog_finds() {
            use crate::git;
            let dir = std::env::temp_dir().join(format!("headway-sample-web-shop-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            build_one(&dir, build_web_shop);
            let log = git::reflog(&dir, "HEAD", 50).unwrap();
            assert_eq!(log[0].action, "reset");
            assert_eq!((log[1].action.as_str(), log[1].message.as_str()), ("commit", "Try a free shipping banner"));
            let on_branches = git::run(&dir, &["branch", "--contains", &log[1].hash]).unwrap();
            assert!(on_branches.trim().is_empty(), "{on_branches}");
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn api_server_has_a_moved_file_and_a_reformat_blame_skips() {
            use crate::git::{self, BlameOptions};
            let dir = std::env::temp_dir().join(format!("headway-sample-api-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            build_one(&dir, build_api);
            let log = git::file_log(&dir, "api/server.py", false, 100).unwrap();
            let seen: Vec<&str> = log.iter().map(|c| c.summary.as_str()).collect();
            assert_eq!(
                seen,
                [
                    "Listen on every interface",
                    "Reformat with four-space indents",
                    "Move the server into an api package",
                    "Answer the orders route",
                    "Serve items over HTTP",
                    "Add server module"
                ]
            );
            let ignore = git::ignore_revs(&dir);
            assert!(ignore.is_some());
            let mut ranges = Vec::new();
            let commits = git::blame(&dir, Some("HEAD"), "api/server.py", &BlameOptions::default(), ignore.as_ref(), |_, r| ranges.push(r)).unwrap();
            assert!(!commits.iter().any(|c| c.summary.starts_with("Reformat")));
            assert!(commits.iter().any(|c| c.summary == "Answer the orders route"));
            assert!(ranges.iter().any(|r| r.path == "server.py"));
            assert!(!git::status(&dir).unwrap().unstaged.is_empty());
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn diff_showcase_has_every_kind_of_change() {
            use crate::app::{DiffDoc, LARGE_FILE_LINES};
            use crate::git;
            let dir = std::env::temp_dir().join(format!("headway-sample-diff-showcase-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            build_one(&dir, build_diff_showcase);
            // The latest commit: a picture, a binary file and a large change.
            let head = git::run(&dir, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
            let d = git::commit_detail(&dir, &head, false).unwrap();
            let file = |p: &str| d.files.iter().find(|f| f.path == p).unwrap();
            assert!(file("assets/badge.png").binary && file("data/model.bin").binary);
            assert!(file("data/rows.csv").added + file("data/rows.csv").removed > LARGE_FILE_LINES);
            // Uncommitted: code with renamed words and a line only re-indented.
            let st = git::status(&dir).unwrap();
            let (unstaged, _) = git::worktree_diffs(&dir, &st, false).unwrap();
            let doc = DiffDoc::new(unstaged, true);
            assert_eq!(doc.files[0].path, "src/cart.rs");
            assert!(doc.marks.values().any(|(r, ws)| !ws && !r.is_empty()), "renamed words are marked");
            assert!(doc.marks.values().any(|(_, ws)| *ws), "a whitespace-only change");
            assert!(crate::highlight::knows("src/cart.rs"));
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn price_rules_stops_in_a_conflicted_merge() {
            use crate::git::{self, Change, ConflictKind, OpType};
            let dir = std::env::temp_dir().join(format!("headway-sample-price-rules-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            build_one(&dir, build_price_rules);
            let op = git::operation(&dir).unwrap().expect("a merge in progress");
            assert_eq!((op.kind, op.ours.as_str(), op.theirs.as_str()), (OpType::Merge, "main", "feature/discounts"));
            let st = git::status(&dir).unwrap();
            let kinds: Vec<(&str, Change)> = st.conflicts.iter().map(|c| (c.path.as_str(), c.change.clone())).collect();
            assert_eq!(
                kinds,
                [
                    ("assets/badge.png", Change::Conflict(ConflictKind::BothModified)),
                    ("src/legacy-tax.js", Change::Conflict(ConflictKind::DeletedByThem)),
                    ("src/pricing.js", Change::Conflict(ConflictKind::BothModified)),
                ]
            );
            assert!(git::conflict_file(&dir, "assets/badge.png", ConflictKind::BothModified).unwrap().binary);
            assert_eq!(git::conflict_file(&dir, "src/pricing.js", ConflictKind::BothModified).unwrap().regions().count(), 3);
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

#[cfg(not(debug_assertions))]
mod imp {
    use crate::workspace::Workspace;
    use std::path::Path;

    pub fn sample_workspace() -> Workspace {
        Workspace::new("My Workspace")
    }

    pub fn samples_missing(_ws: &Workspace) -> bool {
        false
    }

    pub fn build_samples() {}

    pub fn sample_name(_path: &Path) -> Option<&'static str> {
        None
    }

    pub fn reset_sample(path: &Path) -> Result<(), String> {
        Err(format!("{} isn't one of Headway's sample projects", path.display()))
    }
}
