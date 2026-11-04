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

        fn write(&self, file: &str, text: &str) {
            let p = self.dir.join(file);
            if let Some(parent) = p.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(p, text);
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

    const SAMPLES: [(&str, Make); 4] = [
        ("web-shop", build_web_shop),
        ("api-server", build_api),
        ("design-tokens", build_tokens),
        ("price-rules", build_price_rules),
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

    fn build_api(b: &mut Builder) {
        b.write("main.py", "from server import app\n\nif __name__ == '__main__':\n    app.run()\n");
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
        b.write("server.py", "app = None\nDEBUG = False\n");
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
