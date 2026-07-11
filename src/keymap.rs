//! User-configurable keybindings: every UI input is an [`Action`], resolved
//! through a [`Keymap`] (defaults merged with the `[keys]` config table).
//! The command palette dispatches the same actions — one source of truth.

use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use crossterm::event::{KeyCode, KeyModifiers};

use crate::state::StageId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    Help,
    Palette,
    NextTab,
    TabLive,
    TabFindings,
    TabHistory,
    TabPlan,
    Down,
    Up,
    ScrollTop,
    Follow,
    Confirm,
    Cancel,
    CheckFast,
    CheckFull,
    Refresh,
    OpenEditor,
    FeatureNext,
    FeaturePrev,
    Takeover,
    NvimOpen,
    NvimQuickfix,
    RunStage(StageId),
    /// User-defined [commands] entry by index (palette-only).
    Custom(usize),
}

/// (config name, action, palette label) for every nameable action.
pub const ACTIONS: &[(&str, Action, &str)] = &[
    ("quit", Action::Quit, "quit ritual"),
    ("help", Action::Help, "show help"),
    ("palette", Action::Palette, "command palette"),
    ("next-tab", Action::NextTab, "next tab"),
    ("tab-live", Action::TabLive, "go to live tab"),
    ("tab-findings", Action::TabFindings, "go to findings tab"),
    ("tab-history", Action::TabHistory, "go to history tab"),
    ("tab-plan", Action::TabPlan, "go to plan tab"),
    ("down", Action::Down, "move down"),
    ("up", Action::Up, "move up"),
    ("scroll-top", Action::ScrollTop, "scroll to top"),
    ("follow", Action::Follow, "follow stream tail"),
    ("confirm", Action::Confirm, "run selected stage / open item"),
    ("cancel", Action::Cancel, "cancel running stage"),
    ("check-fast", Action::CheckFast, "run check.sh fast"),
    ("check-full", Action::CheckFull, "run full check.sh"),
    ("refresh", Action::Refresh, "refresh auth + artifacts"),
    ("open-editor", Action::OpenEditor, "open in $EDITOR"),
    ("feature-next", Action::FeatureNext, "next feature"),
    ("feature-prev", Action::FeaturePrev, "previous feature"),
    (
        "takeover",
        Action::Takeover,
        "take over run in claude (--resume)",
    ),
    ("nvim-open", Action::NvimOpen, "open in running nvim"),
    (
        "nvim-quickfix",
        Action::NvimQuickfix,
        "send findings to nvim quickfix",
    ),
];

pub fn action_by_name(name: &str) -> Option<Action> {
    ACTIONS
        .iter()
        .find(|(n, _, _)| *n == name)
        .map(|(_, a, _)| *a)
}

/// A pressed key, normalized: alphabetic keys carry case in the char and
/// never a SHIFT modifier (terminals disagree; we normalize both sides).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Chord {
    pub code: KeyCode,
    pub mods: KeyModifiers,
}

impl Chord {
    pub fn normalize(code: KeyCode, mods: KeyModifiers) -> Self {
        let mut mods = mods & (KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT);
        let code = match code {
            KeyCode::Char(c) => {
                if mods.contains(KeyModifiers::SHIFT) && c.is_ascii_alphabetic() {
                    mods.remove(KeyModifiers::SHIFT);
                    KeyCode::Char(c.to_ascii_uppercase())
                } else {
                    KeyCode::Char(c)
                }
            }
            other => other,
        };
        Self { code, mods }
    }
}

/// Parse "ctrl+c", "G", "shift+g", "enter", "alt+up" into a Chord.
pub fn parse_chord(s: &str) -> Result<Chord> {
    let mut mods = KeyModifiers::NONE;
    let parts: Vec<&str> = s.split('+').map(str::trim).collect();
    let (mod_parts, key) = parts.split_at(parts.len().saturating_sub(1));
    let key = key.first().copied().unwrap_or_default();
    for m in mod_parts {
        match m.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods |= KeyModifiers::CONTROL,
            "alt" => mods |= KeyModifiers::ALT,
            "shift" => mods |= KeyModifiers::SHIFT,
            other => bail!("unknown modifier '{other}' in chord '{s}'"),
        }
    }
    let code = match key.to_ascii_lowercase().as_str() {
        "enter" | "return" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "esc" | "escape" => KeyCode::Esc,
        "space" => KeyCode::Char(' '),
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "backspace" => KeyCode::Backspace,
        _ => {
            let mut chars = key.chars();
            let (Some(c), None) = (chars.next(), chars.next()) else {
                bail!("unknown key '{key}' in chord '{s}'");
            };
            KeyCode::Char(c)
        }
    };
    Ok(Chord::normalize(code, mods))
}

#[derive(Debug, Clone)]
pub struct Keymap {
    map: HashMap<Chord, Action>,
}

impl Default for Keymap {
    fn default() -> Self {
        let defaults: &[(&str, &str)] = &[
            ("q", "quit"),
            ("ctrl+c", "quit"),
            ("?", "help"),
            (":", "palette"),
            ("tab", "next-tab"),
            ("1", "tab-live"),
            ("2", "tab-findings"),
            ("3", "tab-history"),
            ("4", "tab-plan"),
            ("j", "down"),
            ("down", "down"),
            ("k", "up"),
            ("up", "up"),
            ("g", "scroll-top"),
            ("G", "follow"),
            ("enter", "confirm"),
            ("x", "cancel"),
            ("c", "check-fast"),
            ("C", "check-full"),
            ("r", "refresh"),
            ("e", "open-editor"),
            ("]", "feature-next"),
            ("[", "feature-prev"),
            ("a", "takeover"),
            ("o", "nvim-open"),
            ("Q", "nvim-quickfix"),
        ];
        let map = defaults
            .iter()
            .map(|(chord, name)| {
                (
                    parse_chord(chord).expect("default chord parses"),
                    action_by_name(name).expect("default action exists"),
                )
            })
            .collect();
        Self { map }
    }
}

impl Keymap {
    /// Apply `[keys]` overrides: `action-name = "chord"`. An unknown action
    /// name is a config error; rebinding replaces that action's default
    /// chord(s) only when the new chord collides.
    pub fn with_overrides(mut self, overrides: &HashMap<String, String>) -> Result<Self> {
        for (name, chord_str) in overrides {
            let action =
                action_by_name(name).with_context(|| format!("[keys]: unknown action '{name}'"))?;
            let chord = parse_chord(chord_str)
                .with_context(|| format!("[keys]: bad chord for '{name}'"))?;
            self.map.insert(chord, action);
        }
        Ok(self)
    }

    pub fn resolve(&self, code: KeyCode, mods: KeyModifiers) -> Option<Action> {
        self.map.get(&Chord::normalize(code, mods)).copied()
    }

    /// The chord(s) bound to an action, for help display.
    pub fn chords_for(&self, action: Action) -> Vec<Chord> {
        let mut v: Vec<Chord> = self
            .map
            .iter()
            .filter(|(_, a)| **a == action)
            .map(|(c, _)| *c)
            .collect();
        v.sort_by_key(|c| format!("{c:?}"));
        v
    }
}

/// Palette entries: nameable actions plus one run entry per pipeline stage.
pub fn palette_entries() -> Vec<(String, Action)> {
    let mut out: Vec<(String, Action)> = ACTIONS
        .iter()
        .filter(|(_, a, _)| !matches!(a, Action::Palette))
        .map(|(_, a, label)| (label.to_string(), *a))
        .collect();
    for id in crate::state::PIPELINE {
        out.push((format!("run {}", id.label()), Action::RunStage(*id)));
    }
    out
}

/// Case-insensitive subsequence match ("rpl" matches "run plan-review").
pub fn fuzzy_match(needle: &str, haystack: &str) -> bool {
    let mut hay = haystack.chars().map(|c| c.to_ascii_lowercase());
    needle
        .chars()
        .map(|c| c.to_ascii_lowercase())
        .filter(|c| !c.is_whitespace())
        .all(|n| hay.any(|h| h == n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_chords() {
        assert_eq!(
            parse_chord("ctrl+c").unwrap(),
            Chord {
                code: KeyCode::Char('c'),
                mods: KeyModifiers::CONTROL
            }
        );
        // shift+g and G normalize identically
        assert_eq!(parse_chord("shift+g").unwrap(), parse_chord("G").unwrap());
        assert_eq!(parse_chord("enter").unwrap().code, KeyCode::Enter);
        assert!(parse_chord("hyper+x").is_err());
        assert!(parse_chord("notakey").is_err());
    }

    #[test]
    fn default_map_resolves() {
        let km = Keymap::default();
        assert_eq!(
            km.resolve(KeyCode::Char('q'), KeyModifiers::NONE),
            Some(Action::Quit)
        );
        assert_eq!(
            km.resolve(KeyCode::Char('c'), KeyModifiers::CONTROL),
            Some(Action::Quit)
        );
        assert_eq!(
            km.resolve(KeyCode::Char('C'), KeyModifiers::SHIFT),
            Some(Action::CheckFull)
        );
        assert_eq!(km.resolve(KeyCode::Char('z'), KeyModifiers::NONE), None);
    }

    #[test]
    fn overrides_apply_and_reject_unknown() {
        let mut o = HashMap::new();
        o.insert("check-full".to_string(), "F".to_string());
        let km = Keymap::default().with_overrides(&o).unwrap();
        assert_eq!(
            km.resolve(KeyCode::Char('F'), KeyModifiers::NONE),
            Some(Action::CheckFull)
        );

        let mut bad = HashMap::new();
        bad.insert("summon-shoggoth".to_string(), "s".to_string());
        assert!(Keymap::default().with_overrides(&bad).is_err());
    }

    #[test]
    fn palette_and_fuzzy() {
        let entries = palette_entries();
        assert!(entries.iter().any(|(l, _)| l == "run plan-review"));
        assert!(fuzzy_match("rpl", "run plan-review"));
        assert!(fuzzy_match("RUN PLAN", "run plan-review"));
        assert!(!fuzzy_match("xyz", "run plan-review"));
        assert!(fuzzy_match("", "anything"));
    }
}
