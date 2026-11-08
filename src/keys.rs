//! Key names from the config ("ctrl-a", "alt-backspace", "f5", "pageup") to actions.

use crate::config::Keys;
use anyhow::{bail, Result};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Up,
    Down,
    PageUp,
    PageDown,
    First,
    Last,
    Resume,
    PrintCommand,
    Quit,
    ClearQuery,
    DeleteWord,
    TogglePreview,
    ToggleHeadless,
    NextSource,
    PrevSource,
    Reindex,
    CursorLeft,
    CursorRight,
    CursorHome,
    CursorEnd,
}

pub struct KeyMap(HashMap<(KeyCode, KeyModifiers), Action>);

impl KeyMap {
    pub fn new(k: &Keys) -> Result<KeyMap> {
        let table: [(&Vec<String>, Action); 20] = [
            (&k.up, Action::Up),
            (&k.down, Action::Down),
            (&k.page_up, Action::PageUp),
            (&k.page_down, Action::PageDown),
            (&k.first, Action::First),
            (&k.last, Action::Last),
            (&k.resume, Action::Resume),
            (&k.print_command, Action::PrintCommand),
            (&k.quit, Action::Quit),
            (&k.clear_query, Action::ClearQuery),
            (&k.delete_word, Action::DeleteWord),
            (&k.toggle_preview, Action::TogglePreview),
            (&k.toggle_headless, Action::ToggleHeadless),
            (&k.next_source, Action::NextSource),
            (&k.prev_source, Action::PrevSource),
            (&k.reindex, Action::Reindex),
            (&k.cursor_left, Action::CursorLeft),
            (&k.cursor_right, Action::CursorRight),
            (&k.cursor_home, Action::CursorHome),
            (&k.cursor_end, Action::CursorEnd),
        ];
        let mut map = HashMap::new();
        for (names, action) in table {
            for name in names {
                map.insert(parse(name)?, action);
            }
        }
        Ok(KeyMap(map))
    }

    pub fn action(&self, ev: &KeyEvent) -> Option<Action> {
        self.0.get(&normalize(ev.code, ev.modifiers)).copied()
    }
}

/// Shift is implied by the character itself ('A') and by BackTab, so it is dropped
/// there; Ctrl+letter is matched case-insensitively.
fn normalize(code: KeyCode, mods: KeyModifiers) -> (KeyCode, KeyModifiers) {
    let mods = mods & (KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT);
    match code {
        KeyCode::Char(c) => {
            let mut m = mods - KeyModifiers::SHIFT;
            let c = if m.contains(KeyModifiers::CONTROL) || m.contains(KeyModifiers::ALT) {
                c.to_ascii_lowercase()
            } else {
                c
            };
            if c == ' ' {
                m = mods;
            }
            (KeyCode::Char(c), m)
        }
        KeyCode::BackTab => (KeyCode::BackTab, mods - KeyModifiers::SHIFT),
        _ => (code, mods),
    }
}

pub fn parse(name: &str) -> Result<(KeyCode, KeyModifiers)> {
    let lower = name.trim().to_lowercase();
    let mut rest = lower.as_str();
    let mut mods = KeyModifiers::NONE;
    loop {
        if rest.len() > 1 {
            if let Some(r) = rest.strip_prefix("ctrl-").or_else(|| rest.strip_prefix("ctrl+")) {
                mods |= KeyModifiers::CONTROL;
                rest = r;
                continue;
            }
            if let Some(r) = rest.strip_prefix("alt-").or_else(|| rest.strip_prefix("alt+")) {
                mods |= KeyModifiers::ALT;
                rest = r;
                continue;
            }
            if let Some(r) = rest.strip_prefix("shift-").or_else(|| rest.strip_prefix("shift+")) {
                mods |= KeyModifiers::SHIFT;
                rest = r;
                continue;
            }
        }
        break;
    }
    let code = match rest {
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" | "pgup" => KeyCode::PageUp,
        "pagedown" | "pgdn" => KeyCode::PageDown,
        "enter" | "return" => KeyCode::Enter,
        "esc" | "escape" => KeyCode::Esc,
        "tab" => KeyCode::Tab,
        "backtab" => KeyCode::BackTab,
        "backspace" | "bs" => KeyCode::Backspace,
        "delete" | "del" => KeyCode::Delete,
        "insert" | "ins" => KeyCode::Insert,
        "space" => KeyCode::Char(' '),
        f if f.starts_with('f') && f.len() > 1 && f[1..].chars().all(|c| c.is_ascii_digit()) => {
            let n: u8 = f[1..].parse()?;
            if !(1..=24).contains(&n) {
                bail!("unknown key {name:?}");
            }
            KeyCode::F(n)
        }
        s if s.chars().count() == 1 => KeyCode::Char(s.chars().next().unwrap()),
        _ => bail!("unknown key {name:?} in [keys]"),
    };
    if code == KeyCode::Tab && mods.contains(KeyModifiers::SHIFT) {
        return Ok(normalize(KeyCode::BackTab, mods));
    }
    Ok(normalize(code, mods))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use ratatui::crossterm::event::KeyEvent;

    #[test]
    fn defaults_parse_and_match() {
        let km = KeyMap::new(&Config::default_config().keys).unwrap();
        let ev = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL);
        assert_eq!(km.action(&ev), Some(Action::ToggleHeadless));
        let ev = KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT);
        assert_eq!(km.action(&ev), Some(Action::PrevSource));
        let ev = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        assert_eq!(km.action(&ev), None, "plain letters go to the search");
        let ev = KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT);
        assert_eq!(km.action(&ev), None);
    }

    #[test]
    fn names() {
        assert_eq!(parse("ctrl-home").unwrap(), (KeyCode::Home, KeyModifiers::CONTROL));
        assert_eq!(parse("F5").unwrap(), (KeyCode::F(5), KeyModifiers::NONE));
        assert_eq!(parse("shift-tab").unwrap(), (KeyCode::BackTab, KeyModifiers::NONE));
        assert_eq!(parse("alt-x").unwrap(), (KeyCode::Char('x'), KeyModifiers::ALT));
        assert!(parse("hyper-q").is_err());
    }
}
