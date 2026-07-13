//! In-TUI settings editor plumbing: a declarative catalog of the practical
//! config knobs, per-key layer provenance, and input validation. The
//! comment-preserving project-config writer lives here too so the catalog,
//! validator, and writer can never drift apart.
//!
//! Scope is deliberately the "practical set": budgets, model/effort routing,
//! appearance, and behavior toggles. Command seams (`claude_cmd`, …),
//! `[keys]`, `[commands]`, and the sub-tool tables stay file-only.

use std::path::Path;

use crate::config::Config;
use crate::theme::IconSet;

/// What a setting holds; drives the editing UX and validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingKind {
    Bool,
    F64,
    /// Clearable number — empty input removes the key (daily budget).
    OptF64,
    U64,
    Text,
    /// Clearable string — empty input removes the key.
    OptText,
    Enum(&'static [&'static str]),
    /// Clearable enum — cycling past the last variant unsets the key.
    OptEnum(&'static [&'static str]),
}

/// One editable knob. `key` is the dotted TOML path exactly as it appears in
/// the config file; the keystone test proves every key round-trips through
/// `FileConfig`'s `deny_unknown_fields`.
pub struct SettingDef {
    pub key: &'static str,
    pub label: &'static str,
    pub doc: &'static str,
    pub group: &'static str,
    pub kind: SettingKind,
    /// Effective (post-layering) value for display; None = unset Opt*.
    pub get: fn(&Config) -> Option<String>,
}

const EFFORTS: &[&str] = &["low", "medium", "high", "xhigh"];
const THEMES: &[&str] = &["eldritch", "tokyonight"];
const ICONS: &[&str] = &["nerd", "ascii"];

pub static CATALOG: &[SettingDef] = &[
    // -- budgets ------------------------------------------------------------
    SettingDef {
        key: "budget_plan_review_usd",
        label: "plan-review",
        doc: "USD cap for one plan-review run",
        group: "budgets",
        kind: SettingKind::F64,
        get: |c| Some(c.budget_plan_review_usd.to_string()),
    },
    SettingDef {
        key: "budget_dual_review_usd",
        label: "dual-review",
        doc: "USD cap for one dual-review run",
        group: "budgets",
        kind: SettingKind::F64,
        get: |c| Some(c.budget_dual_review_usd.to_string()),
    },
    SettingDef {
        key: "budget_doc_chat_usd",
        label: "doc chat",
        doc: "USD cap per spec/plan chat message",
        group: "budgets",
        kind: SettingKind::F64,
        get: |c| Some(c.budget_doc_chat_usd.to_string()),
    },
    SettingDef {
        key: "budget_finding_fix_usd",
        label: "finding fix (per run)",
        doc: "USD cap for ONE F-apply batch fix run",
        group: "budgets",
        kind: SettingKind::F64,
        get: |c| Some(c.budget_finding_fix_usd.to_string()),
    },
    SettingDef {
        key: "budget_daily_usd",
        label: "daily ceiling",
        doc: "daily spend across all runs (empty = unlimited)",
        group: "budgets",
        kind: SettingKind::OptF64,
        get: |c| c.budget_daily_usd.map(|v| v.to_string()),
    },
    // -- routing ------------------------------------------------------------
    SettingDef {
        key: "models.plan",
        label: "model: plan",
        doc: "model for the plan stage (e.g. fable-5, opus)",
        group: "routing",
        kind: SettingKind::OptText,
        get: |c| c.models.get("plan").cloned(),
    },
    SettingDef {
        key: "models.plan-review",
        label: "model: plan-review",
        doc: "model for plan-review runs",
        group: "routing",
        kind: SettingKind::OptText,
        get: |c| c.models.get("plan-review").cloned(),
    },
    SettingDef {
        key: "models.dual-review",
        label: "model: dual-review",
        doc: "model for dual-review runs",
        group: "routing",
        kind: SettingKind::OptText,
        get: |c| c.models.get("dual-review").cloned(),
    },
    SettingDef {
        key: "fallback_model",
        label: "fallback models",
        doc: "comma-separated fallbacks when the pinned model errors",
        group: "routing",
        kind: SettingKind::OptText,
        get: |c| c.fallback_model.clone(),
    },
    SettingDef {
        key: "effort.plan",
        label: "effort: plan",
        doc: "reasoning effort for the plan stage",
        group: "routing",
        kind: SettingKind::OptEnum(EFFORTS),
        get: |c| c.effort.get("plan").cloned(),
    },
    SettingDef {
        key: "effort.plan-fix",
        label: "effort: plan-fix",
        doc: "reasoning effort for F-apply batch fixes",
        group: "routing",
        kind: SettingKind::OptEnum(EFFORTS),
        get: |c| c.effort.get("plan-fix").cloned(),
    },
    SettingDef {
        key: "effort.plan-review",
        label: "effort: plan-review",
        doc: "reasoning effort for plan-review runs",
        group: "routing",
        kind: SettingKind::OptEnum(EFFORTS),
        get: |c| c.effort.get("plan-review").cloned(),
    },
    SettingDef {
        key: "effort.dual-review",
        label: "effort: dual-review",
        doc: "reasoning effort for dual-review runs",
        group: "routing",
        kind: SettingKind::OptEnum(EFFORTS),
        get: |c| c.effort.get("dual-review").cloned(),
    },
    // -- appearance ----------------------------------------------------------
    SettingDef {
        key: "theme",
        label: "theme",
        doc: "color theme",
        group: "appearance",
        kind: SettingKind::Enum(THEMES),
        get: |c| Some(c.theme_name.clone()),
    },
    SettingDef {
        key: "icons",
        label: "icons",
        doc: "icon set (nerd fonts or plain ascii)",
        group: "appearance",
        kind: SettingKind::Enum(ICONS),
        get: |c| {
            Some(
                match c.theme.icons {
                    IconSet::Nerd => "nerd",
                    IconSet::Ascii => "ascii",
                }
                .to_string(),
            )
        },
    },
    SettingDef {
        key: "transparency",
        label: "transparency",
        doc: "terminal background shows through the main pane",
        group: "appearance",
        kind: SettingKind::Bool,
        get: |c| Some(c.theme.transparency.to_string()),
    },
    // -- behavior -----------------------------------------------------------
    SettingDef {
        key: "notifications",
        label: "notifications",
        doc: "desktop notifications on stage completion",
        group: "behavior",
        kind: SettingKind::Bool,
        get: |c| Some(c.notifications.to_string()),
    },
    SettingDef {
        key: "redaction",
        label: "redaction",
        doc: "redact secrets from archives/streams/reports",
        group: "behavior",
        kind: SettingKind::Bool,
        get: |c| Some(c.redaction.to_string()),
    },
    SettingDef {
        key: "offline",
        label: "offline",
        doc: "skip cloud auth preflights/probes",
        group: "behavior",
        kind: SettingKind::Bool,
        get: |c| Some(c.offline.to_string()),
    },
    SettingDef {
        key: "base_ref",
        label: "base ref",
        doc: "git ref diffs are computed against",
        group: "behavior",
        kind: SettingKind::Text,
        get: |c| Some(c.base_ref.clone()),
    },
    SettingDef {
        key: "check_timeout_secs",
        label: "check timeout (s)",
        doc: "hard ceiling on any check.sh run, in seconds",
        group: "behavior",
        kind: SettingKind::U64,
        get: |c| Some(c.check_timeout_secs.to_string()),
    },
];

/// Which config layer defines a key. Project overrides user overrides
/// defaults; the env seams cover only `*_cmd` values, none in the catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Default,
    User,
    Project,
}

impl Source {
    pub fn tag(&self) -> &'static str {
        match self {
            Source::Default => "default",
            Source::User => "user",
            Source::Project => "project",
        }
    }
}

/// Per-key provenance: parse each layer file raw (no serde structs) and check
/// dotted-key presence, project first. Explicit paths keep this testable.
pub fn source_of(user_path: Option<&Path>, project_path: &Path, key: &str) -> Source {
    if file_defines(project_path, key) {
        return Source::Project;
    }
    if let Some(user) = user_path
        && file_defines(user, key)
    {
        return Source::User;
    }
    Source::Default
}

/// Unreadable or unparseable layer files count as not defining the key —
/// provenance is a display hint, never a gate.
fn file_defines(path: &Path, key: &str) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(doc) = text.parse::<toml_edit::DocumentMut>() else {
        return false;
    };
    let mut parts = key.split('.').peekable();
    let mut tbl: &dyn toml_edit::TableLike = doc.as_table();
    while let Some(part) = parts.next() {
        let Some(item) = tbl.get(part) else {
            return false;
        };
        if parts.peek().is_none() {
            return !item.is_none();
        }
        match item.as_table_like() {
            Some(t) => tbl = t,
            None => return false,
        }
    }
    false
}

/// A validated value ready for the writer.
#[derive(Debug, Clone, PartialEq)]
pub enum SettingValue {
    F64(f64),
    U64(u64),
    Bool(bool),
    Str(String),
}

impl std::fmt::Display for SettingValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SettingValue::F64(v) => write!(f, "{v}"),
            SettingValue::U64(v) => write!(f, "{v}"),
            SettingValue::Bool(v) => write!(f, "{v}"),
            SettingValue::Str(v) => write!(f, "{v}"),
        }
    }
}

/// Validate raw prompt input for a kind. `Ok(None)` = clear the key (only
/// for Opt* kinds); errors are user-facing one-liners shown in the prompt.
pub fn validate(kind: &SettingKind, input: &str) -> Result<Option<SettingValue>, String> {
    let s = input.trim();
    if s.is_empty() {
        return match kind {
            SettingKind::OptF64 | SettingKind::OptText | SettingKind::OptEnum(_) => Ok(None),
            _ => Err("a value is required (esc to cancel)".into()),
        };
    }
    match kind {
        SettingKind::Bool => match s {
            "true" => Ok(Some(SettingValue::Bool(true))),
            "false" => Ok(Some(SettingValue::Bool(false))),
            _ => Err("true or false".into()),
        },
        SettingKind::F64 | SettingKind::OptF64 => s
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite() && *v > 0.0)
            .map(|v| Some(SettingValue::F64(v)))
            .ok_or_else(|| "must be a number > 0".into()),
        SettingKind::U64 => s
            .parse::<u64>()
            .ok()
            .filter(|v| *v >= 1)
            .map(|v| Some(SettingValue::U64(v)))
            .ok_or_else(|| "must be a whole number >= 1".into()),
        SettingKind::Text | SettingKind::OptText => Ok(Some(SettingValue::Str(s.to_string()))),
        SettingKind::Enum(variants) | SettingKind::OptEnum(variants) => {
            if variants.contains(&s) {
                Ok(Some(SettingValue::Str(s.to_string())))
            } else {
                Err(format!("one of: {}", variants.join(", ")))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get(key: &str, cfg: &Config) -> Option<String> {
        (CATALOG.iter().find(|d| d.key == key).expect(key).get)(cfg)
    }

    /// The safety keystone: every key the writer can ever emit must be
    /// accepted by FileConfig's deny_unknown_fields.
    #[test]
    fn every_catalog_key_is_accepted_by_file_config() {
        let mut doc = toml_edit::DocumentMut::new();
        for def in CATALOG {
            let item = match def.kind {
                SettingKind::Bool => toml_edit::value(true),
                SettingKind::F64 | SettingKind::OptF64 => toml_edit::value(1.5),
                SettingKind::U64 => toml_edit::value(60i64),
                SettingKind::Text | SettingKind::OptText => toml_edit::value("x"),
                SettingKind::Enum(vs) | SettingKind::OptEnum(vs) => toml_edit::value(vs[0]),
            };
            let mut parts = def.key.split('.').collect::<Vec<_>>();
            let leaf = parts.pop().expect("non-empty key");
            let mut tbl = doc.as_table_mut();
            for part in parts {
                tbl = tbl
                    .entry(part)
                    .or_insert(toml_edit::table())
                    .as_table_mut()
                    .expect("intermediate is a table");
            }
            tbl[leaf] = item;
        }
        let text = doc.to_string();
        let parsed: Result<crate::config::FileConfig, _> = toml::from_str(&text);
        assert!(
            parsed.is_ok(),
            "deny_unknown_fields rejected a catalog key:\n{text}\n{:?}",
            parsed.err()
        );
    }

    #[test]
    fn getters_reflect_effective_config() {
        let mut cfg = Config {
            budget_finding_fix_usd: 3.0,
            ..Config::default()
        };
        cfg.models.insert("plan".into(), "fable-5".into());
        cfg.effort.insert("plan".into(), "xhigh".into());
        assert_eq!(get("budget_finding_fix_usd", &cfg), Some("3".into()));
        assert_eq!(get("budget_daily_usd", &cfg), None);
        assert_eq!(get("models.plan", &cfg), Some("fable-5".into()));
        assert_eq!(get("models.plan-review", &cfg), None);
        assert_eq!(get("effort.plan", &cfg), Some("xhigh".into()));
        assert_eq!(get("theme", &cfg), Some("eldritch".into()));
        assert_eq!(get("icons", &cfg), Some("nerd".into()));
        assert_eq!(get("transparency", &cfg), Some("true".into()));
        assert_eq!(get("base_ref", &cfg), Some("main".into()));
        assert_eq!(get("check_timeout_secs", &cfg), Some("600".into()));
    }

    #[test]
    fn source_of_walks_layers_project_first() {
        let tmp = tempfile::tempdir().unwrap();
        let user = tmp.path().join("user.toml");
        let project = tmp.path().join("project.toml");

        // Neither file exists.
        assert_eq!(source_of(Some(&user), &project, "theme"), Source::Default);

        std::fs::write(&user, "theme = \"tokyonight\"\n").unwrap();
        assert_eq!(source_of(Some(&user), &project, "theme"), Source::User);

        std::fs::write(
            &project,
            "theme = \"eldritch\"\n\n[effort]\nplan = \"xhigh\"\n",
        )
        .unwrap();
        assert_eq!(source_of(Some(&user), &project, "theme"), Source::Project);
        assert_eq!(
            source_of(Some(&user), &project, "effort.plan"),
            Source::Project
        );
        assert_eq!(
            source_of(Some(&user), &project, "effort.plan-fix"),
            Source::Default
        );
        assert_eq!(source_of(None, &project, "notifications"), Source::Default);
        // A scalar is not a table: dotted lookup under it must not panic.
        assert_eq!(source_of(None, &project, "theme.nested"), Source::Default);
    }

    #[test]
    fn validate_matrix() {
        use SettingKind::*;
        assert!(validate(&F64, "abc").is_err());
        assert!(validate(&F64, "-1").is_err());
        assert!(validate(&F64, "0").is_err());
        assert!(validate(&F64, "").is_err());
        assert_eq!(validate(&F64, "4.5"), Ok(Some(SettingValue::F64(4.5))));
        assert_eq!(validate(&OptF64, ""), Ok(None));
        assert_eq!(validate(&OptF64, "2"), Ok(Some(SettingValue::F64(2.0))));
        assert!(validate(&U64, "0").is_err());
        assert!(validate(&U64, "9.5").is_err());
        assert_eq!(validate(&U64, "900"), Ok(Some(SettingValue::U64(900))));
        assert_eq!(
            validate(&OptEnum(EFFORTS), "xhigh"),
            Ok(Some(SettingValue::Str("xhigh".into())))
        );
        assert!(validate(&OptEnum(EFFORTS), "ultra").is_err());
        assert_eq!(validate(&OptEnum(EFFORTS), ""), Ok(None));
        assert!(validate(&Enum(THEMES), "").is_err());
        assert!(validate(&Text, "").is_err());
        assert!(validate(&Text, "   ").is_err());
        assert_eq!(
            validate(&Text, " main "),
            Ok(Some(SettingValue::Str("main".into())))
        );
        assert_eq!(validate(&OptText, ""), Ok(None));
        assert_eq!(validate(&Bool, "true"), Ok(Some(SettingValue::Bool(true))));
        assert!(validate(&Bool, "yes").is_err());
    }
}
