//! Keyboard shortcuts: reading them from text ("Ctrl+Shift+P"), showing them, and the
//! table of what's bound to what (the defaults from `actions`, changed by the
//! "keybindings" section of the settings file).
//!
//! "Ctrl" means the Command key on a Mac. Writing `"none"` (or an empty string) as an
//! action's shortcut removes it.

use crate::actions::{self, Id, Scope};
use egui::{Context, Event, InputState, Key, Modifiers};
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Shortcut {
    pub command: bool,
    pub alt: bool,
    pub shift: bool,
    pub key: Key,
}

impl Shortcut {
    /// Keys that need Shift to type on most layouts: Shift isn't part of what's asked for.
    fn shift_is_implied(&self) -> bool {
        matches!(self.key, Key::Questionmark | Key::Colon | Key::Plus | Key::Pipe)
    }

    /// A key you'd type into a text box too (no Ctrl/Cmd or Alt held).
    pub fn is_typing(&self) -> bool {
        let name = self.key.name();
        let function_key = name.len() > 1 && name.starts_with('F') && name[1..].chars().all(|c| c.is_ascii_digit());
        !self.command && !self.alt && !function_key
    }

    pub fn modifiers(&self) -> Modifiers {
        Modifiers { alt: self.alt, shift: self.shift, command: self.command, ..Default::default() }
    }

    pub fn matches(&self, m: &Modifiers, key: Key) -> bool {
        key == self.key && m.command == self.command && m.alt == self.alt && (m.shift == self.shift || self.shift_is_implied())
    }

    /// How it's written in the settings file.
    pub fn text(&self) -> String {
        let mut s = String::new();
        if self.command {
            s.push_str("Ctrl+");
        }
        if self.alt {
            s.push_str("Alt+");
        }
        if self.shift {
            s.push_str("Shift+");
        }
        s.push_str(&key_text(self.key));
        s
    }

    /// How it's shown to people: ⌘⇧P on a Mac, Ctrl+Shift+P elsewhere.
    pub fn show(&self, ctx: &Context) -> String {
        if self.is_typing() && !self.shift {
            return key_text(self.key);
        }
        ctx.format_shortcut(&egui::KeyboardShortcut::new(self.modifiers(), self.key))
    }
}

/// A key as typed in the settings file: its symbol when that's plain ASCII ("?"), else its name.
fn key_text(k: Key) -> String {
    let s = k.symbol_or_name();
    if s.is_ascii() { s.to_string() } else { k.name().to_string() }
}

/// Read "Ctrl+Shift+P", "Alt+ArrowUp", "?" or "S".
pub fn parse(text: &str) -> Result<Shortcut, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("empty".into());
    }
    // "+" separates the parts, but the Plus key is written "+" too ("Ctrl++").
    let (mods_text, key_text) = if text == "+" {
        ("", "+")
    } else if let Some(rest) = text.strip_suffix("++") {
        (rest, "+")
    } else {
        text.rsplit_once('+').unwrap_or(("", text))
    };
    let mods: Vec<&str> = mods_text.split('+').map(str::trim).filter(|m| !m.is_empty()).collect();
    let mut s = Shortcut { command: false, alt: false, shift: false, key: Key::A };
    for m in &mods {
        match m.to_ascii_lowercase().as_str() {
            "ctrl" | "control" | "cmd" | "command" | "mod" | "meta" | "super" => s.command = true,
            "alt" | "option" | "opt" => s.alt = true,
            "shift" => s.shift = true,
            other => return Err(format!("unknown modifier “{other}”")),
        }
    }
    let alias = match key_text.to_ascii_lowercase().as_str() {
        "up" => Some(Key::ArrowUp),
        "down" => Some(Key::ArrowDown),
        "left" => Some(Key::ArrowLeft),
        "right" => Some(Key::ArrowRight),
        "esc" => Some(Key::Escape),
        "return" => Some(Key::Enter),
        "pgup" => Some(Key::PageUp),
        "pgdn" | "pgdown" => Some(Key::PageDown),
        _ => None,
    };
    s.key = alias.or_else(|| Key::from_name(key_text)).ok_or_else(|| format!("unknown key “{key_text}”"))?;
    if s.key == Key::Questionmark {
        s.shift = false;
    }
    Ok(s)
}

/// Take the key press from this frame's input if it is this shortcut.
pub fn consume(i: &mut InputState, s: &Shortcut) -> bool {
    let at = i.events.iter().position(|e| matches!(e, Event::Key { key, pressed: true, repeat: false, modifiers, .. } if s.matches(modifiers, *key)));
    match at {
        Some(n) => {
            i.events.remove(n);
            // A key you'd type also sends its character; don't let a focused box get it too.
            if s.is_typing() {
                let typed = key_text(s.key);
                if let Some(t) = i.events.iter().position(|e| matches!(e, Event::Text(t) if t.eq_ignore_ascii_case(&typed))) {
                    i.events.remove(t);
                }
            }
            true
        }
        None => false,
    }
}

/// Which action each shortcut runs.
#[derive(Clone, Debug, Default)]
pub struct Bindings {
    /// In the order they should be tried: most modifiers first, so Ctrl+Shift+P is
    /// never taken for Ctrl+P.
    list: Vec<(Shortcut, Id)>,
    by_action: Vec<(Id, Vec<Shortcut>)>,
    /// What was wrong in the settings file (ignored entries).
    pub problems: Vec<String>,
}

impl Bindings {
    pub fn defaults() -> Self {
        Self::from_settings(&BTreeMap::new())
    }

    pub fn from_settings(custom: &BTreeMap<String, String>) -> Self {
        let defaults: Vec<(Id, Vec<Shortcut>)> = actions::DEFS.iter().map(|d| (d.id, d.shortcuts.iter().filter_map(|s| parse(s).ok()).collect())).collect();
        let mut by_action = defaults.clone();
        let mut problems = Vec::new();
        // Every valid change goes in first, so swapping two shortcuts works whatever the
        // order of their names; then each one is checked against the lot.
        let mut changed: Vec<(Id, &str, Shortcut)> = Vec::new();
        for (name, text) in custom {
            let Some(def) = actions::by_key(name) else {
                problems.push(format!("keybindings: there's no action called “{name}”"));
                continue;
            };
            let wanted = if matches!(text.trim().to_ascii_lowercase().as_str(), "" | "none") {
                Vec::new()
            } else {
                match parse(text) {
                    Ok(s) => vec![s],
                    Err(e) => {
                        problems.push(format!("keybindings: “{name}”: {e}"));
                        continue;
                    }
                }
            };
            if let Some(s) = wanted.first() {
                changed.push((def.id, name, *s));
            }
            if let Some(slot) = by_action.iter_mut().find(|(id, _)| *id == def.id) {
                slot.1 = wanted;
            }
        }
        // A shortcut put back to its default may itself clash with another change: look again until none does.
        let mut again = true;
        while std::mem::take(&mut again) {
        for (id, name, s) in &changed {
            let (id, name, s) = (*id, *name, *s);
            if by_action.iter().find(|(i, _)| *i == id).is_none_or(|(_, v)| v.first() != Some(&s)) {
                continue;
            }
            if let Some(other) = clash(&by_action, id, &s) {
                again = true;
                problems.push(format!("keybindings: {} is already used by “{}”, so “{name}” keeps its own shortcut", s.text(), actions::def_of(other).title));
                if let (Some(slot), Some(dflt)) = (by_action.iter_mut().find(|(i, _)| *i == id), defaults.iter().find(|(i, _)| *i == id)) {
                    slot.1 = dflt.1.clone();
                }
            }
        }
        }
        let mut list: Vec<(Shortcut, Id)> = by_action.iter().flat_map(|(id, v)| v.iter().map(|s| (*s, *id))).collect();
        list.sort_by_key(|(s, _)| std::cmp::Reverse(usize::from(s.command) + usize::from(s.alt) + usize::from(s.shift)));
        Self { list, by_action, problems }
    }

    pub fn of(&self, id: Id) -> &[Shortcut] {
        self.by_action.iter().find(|(i, _)| *i == id).map(|(_, v)| v.as_slice()).unwrap_or(&[])
    }

    /// The action whose shortcut was pressed this frame (and take the key press), if
    /// any of the `allowed` ones was.
    pub fn pressed(&self, i: &mut InputState, allowed: impl Fn(Id, &Shortcut) -> bool) -> Option<Id> {
        for (s, id) in &self.list {
            if actions::def_of(*id).scope == Scope::CommitBox || !allowed(*id, s) {
                continue;
            }
            if consume(i, s) {
                return Some(*id);
            }
        }
        None
    }

    /// The first shortcut of an action, as shown to people ("" if it has none).
    pub fn hint(&self, ctx: &Context, id: Id) -> String {
        self.of(id).first().map(|s| s.show(ctx)).unwrap_or_default()
    }
}

/// An action that already uses this shortcut where it would get in the way.
fn clash(table: &[(Id, Vec<Shortcut>)], me: Id, s: &Shortcut) -> Option<Id> {
    let my_scope = actions::def_of(me).scope;
    table.iter().filter(|(id, _)| *id != me).find_map(|(id, list)| {
        let their_scope = actions::def_of(*id).scope;
        // The staging keys only apply in the list of changes; the commit box only when it has focus.
        let overlap = my_scope == their_scope || my_scope == Scope::Anywhere || their_scope == Scope::Anywhere;
        let same = list.iter().any(|t| t.key == s.key && t.command == s.command && t.alt == s.alt && (t.shift == s.shift || t.shift_is_implied()));
        (overlap && same).then_some(*id)
    })
}

/// The bindings in force, shared with every view through the egui context.
pub fn publish(ctx: &Context, b: &Arc<Bindings>) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::NULL.with("keybindings"), b.clone()));
}

fn shared(ctx: &Context) -> Arc<Bindings> {
    ctx.data(|d| d.get_temp::<Arc<Bindings>>(egui::Id::NULL.with("keybindings"))).unwrap_or_else(|| Arc::new(Bindings::defaults()))
}

/// Note whether the keyboard or the mouse was used last (call once a frame): focus rings
/// only show while the keyboard is in use.
pub fn note_input(ctx: &Context) {
    let key = egui::Id::NULL.with("keyboard_mode");
    let (typed, clicked) = ctx.input(|i| {
        let typed = i.events.iter().any(|e| matches!(e, Event::Key { pressed: true, key, .. } if !matches!(key, Key::Escape)));
        (typed, i.pointer.any_pressed())
    });
    if typed || clicked {
        ctx.data_mut(|d| d.insert_temp(key, typed));
    }
}

/// Whether the keyboard was used since the mouse last was.
pub fn keyboard_mode(ctx: &Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(egui::Id::NULL.with("keyboard_mode"))).unwrap_or(false)
}

/// An action's shortcut for a tooltip: "Ctrl+B", or "" when it has none.
pub fn hint(ctx: &Context, id: Id) -> String {
    shared(ctx).hint(ctx, id)
}

/// A tooltip with its shortcut: "Hide the sidebar (Ctrl+B)".
pub fn with_hint(ctx: &Context, text: &str, id: Id) -> String {
    match hint(ctx, id) {
        h if h.is_empty() => text.to_string(),
        h => format!("{text} ({h})"),
    }
}

/// Whether the shortcut of this action was pressed this frame (for the few actions
/// that only work while one widget has focus).
pub fn pressed_for(ctx: &Context, id: Id) -> bool {
    let b = shared(ctx);
    ctx.input_mut(|i| b.of(id).iter().any(|s| consume(i, s)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_what_the_table_uses() {
        let s = parse("Ctrl+Shift+P").unwrap();
        assert!(s.command && s.shift && !s.alt && s.key == Key::P);
        assert_eq!(parse("cmd+k").unwrap(), parse("Ctrl+K").unwrap());
        assert_eq!(parse("Alt+Up").unwrap().key, Key::ArrowUp);
        assert_eq!(parse("S").unwrap(), Shortcut { command: false, alt: false, shift: false, key: Key::S });
        assert_eq!(parse("?").unwrap().key, Key::Questionmark);
        assert_eq!(parse("Ctrl+Enter").unwrap().key, Key::Enter);
        assert_eq!(parse("Ctrl+PageDown").unwrap().key, Key::PageDown);
        assert_eq!(parse("Ctrl++").unwrap().key, Key::Plus);
        assert!(parse("Ctrl+Hyper+P").unwrap_err().contains("modifier"));
        assert!(parse("Ctrl+Banana").unwrap_err().contains("key"));
        assert!(parse("").is_err());
    }

    #[test]
    fn text_round_trips() {
        for s in ["Ctrl+Shift+P", "Ctrl+K", "Alt+Up", "S", "?", "Ctrl+PageDown", "Ctrl+Enter"] {
            assert_eq!(parse(s).unwrap().text(), s);
            assert_eq!(parse(&parse(s).unwrap().text()).unwrap(), parse(s).unwrap());
        }
    }

    #[test]
    fn matching_is_exact_about_modifiers() {
        let p = parse("Ctrl+P").unwrap();
        let ctrl = Modifiers::COMMAND;
        let ctrl_shift = Modifiers::COMMAND | Modifiers::SHIFT;
        assert!(p.matches(&ctrl, Key::P));
        assert!(!p.matches(&ctrl_shift, Key::P), "Ctrl+Shift+P isn't Ctrl+P");
        assert!(!p.matches(&Modifiers::NONE, Key::P));
        let q = parse("?").unwrap();
        assert!(q.matches(&Modifiers::SHIFT, Key::Questionmark) && q.matches(&Modifiers::NONE, Key::Questionmark));
        assert!(!q.matches(&ctrl, Key::Questionmark));
    }

    #[test]
    fn the_defaults_are_all_valid_and_never_clash() {
        for d in actions::DEFS {
            for s in d.shortcuts {
                assert!(parse(s).is_ok(), "{} has a bad default {s}", d.key);
            }
        }
        let b = Bindings::defaults();
        assert!(b.problems.is_empty());
        for (id, list) in &b.by_action {
            for s in list {
                assert_eq!(clash(&b.by_action, *id, s), None, "{} clashes", actions::def_of(*id).key);
            }
        }
    }

    #[test]
    fn more_modifiers_are_tried_first() {
        let b = Bindings::defaults();
        let pos = |t: &str| b.list.iter().position(|(s, _)| *s == parse(t).unwrap()).unwrap();
        assert!(pos("Ctrl+Shift+P") < pos("Ctrl+P"));
    }

    #[test]
    fn settings_rebind_remove_and_report_problems() {
        let mut custom = BTreeMap::new();
        custom.insert("git.fetch".to_string(), "Ctrl+Shift+F".to_string());
        custom.insert("view.refresh".to_string(), "none".to_string());
        custom.insert("view.sidebar".to_string(), "Ctrl+K".to_string()); // used by the palette
        custom.insert("view.search".to_string(), "Ctrl+Banana".to_string());
        custom.insert("no.such.action".to_string(), "Ctrl+X".to_string());
        let b = Bindings::from_settings(&custom);
        assert_eq!(b.of(Id::Fetch), &[parse("Ctrl+Shift+F").unwrap()]);
        assert!(b.of(Id::Refresh).is_empty(), "removed");
        assert_eq!(b.of(Id::ToggleSidebar), &[parse("Ctrl+B").unwrap()], "a clash keeps the default");
        assert_eq!(b.of(Id::FocusSearch), &[parse("Ctrl+F").unwrap()], "a bad key keeps the default");
        assert_eq!(b.problems.len(), 3, "{:?}", b.problems);
        assert!(b.problems.iter().any(|p| p.contains("already used")));
        assert!(b.problems.iter().any(|p| p.contains("no action called")));
    }

    #[test]
    fn shortcuts_can_be_swapped_whatever_the_order_of_their_names() {
        // Refresh and Fetch trade places: neither clashes once both have moved.
        let mut custom = BTreeMap::new();
        custom.insert("git.fetch".to_string(), "Ctrl+R".to_string());
        custom.insert("view.refresh".to_string(), "Ctrl+Shift+R".to_string());
        let b = Bindings::from_settings(&custom);
        assert!(b.problems.is_empty(), "{:?}", b.problems);
        assert_eq!(b.of(Id::Fetch), &[parse("Ctrl+R").unwrap()]);
        assert_eq!(b.of(Id::Refresh), &[parse("Ctrl+Shift+R").unwrap()]);
        // Taking another action's key while it keeps it is still a clash.
        let mut custom = BTreeMap::new();
        custom.insert("git.fetch".to_string(), "Ctrl+R".to_string());
        let b = Bindings::from_settings(&custom);
        assert_eq!(b.problems.len(), 1);
        assert!(b.of(Id::Fetch).is_empty());
        assert_eq!(b.of(Id::Refresh), &[parse("Ctrl+R").unwrap()]);
        // A default put back isn't allowed to clash either: both lose.
        let mut custom = BTreeMap::new();
        custom.insert("view.refresh".to_string(), "Ctrl+K".to_string());
        custom.insert("git.fetch".to_string(), "Ctrl+R".to_string());
        let b = Bindings::from_settings(&custom);
        let ctrl_r = parse("Ctrl+R").unwrap();
        assert!(b.list.iter().filter(|(s, _)| *s == ctrl_r).count() <= 1, "{:?}", b.list);
        // Freeing a key in the same file lets another action have it.
        let mut custom = BTreeMap::new();
        custom.insert("git.fetch".to_string(), "Ctrl+R".to_string());
        custom.insert("view.refresh".to_string(), "none".to_string());
        assert!(Bindings::from_settings(&custom).problems.is_empty());
    }

    #[test]
    fn single_keys_may_be_reused_across_scopes_but_not_with_a_global() {
        // S is the staging key; binding another staging action to it clashes, a global one too.
        let mut custom = BTreeMap::new();
        custom.insert("stage.unstage-file".to_string(), "S".to_string());
        assert!(Bindings::from_settings(&custom).problems.iter().any(|p| p.contains("already used")));
        let mut custom = BTreeMap::new();
        custom.insert("git.fetch".to_string(), "S".to_string());
        assert!(Bindings::from_settings(&custom).problems.iter().any(|p| p.contains("already used")));
    }
}
