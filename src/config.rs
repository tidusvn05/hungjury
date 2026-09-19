//! `hungjury.toml` loading and CLI/TOML/default merge.
//!
//! Merge order: defaults → `~/.config/hungjury/config.toml` →
//! `./hungjury.toml` → selected profile → CLI flags → PATH auto-detection
//! for `jurors`/`judge` when nobody configured them.
//!
//! Data dir: `$HUNGJURY_HOME` when set, else
//! `directories::ProjectDirs` (`~/.local/share/hungjury`). It holds
//! `memory.db`, `cache/`, `calls.jsonl`, `state.json`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::backend::BackendKind;
use crate::error::{Error, Result};

/// What to do when the jury hangs on one or more questions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Escalate {
    /// Call the judge immediately; its answers replace the hung ones.
    #[default]
    Sync,
    /// Return the jury result and enqueue the decision for `learn`.
    Queue,
    /// Return the jury result; hung questions stay unresolved (exit 2).
    Off,
}

/// `[limits]` section.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LimitsConfig {
    /// Per-juror timeout for a text state, seconds.
    pub juror_timeout_secs: u64,
    /// Per-juror timeout for a workspace state, seconds.
    pub juror_workspace_timeout_secs: u64,
    /// Judge call timeout, seconds.
    pub judge_timeout_secs: u64,
    /// Parse/validate retries per agent call.
    pub retry_attempts: u32,
    /// Max concurrent CLI calls.
    pub max_concurrency: usize,
    /// Max CLI calls per calendar day.
    pub daily_cap: u32,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            juror_timeout_secs: 60,
            juror_workspace_timeout_secs: 300,
            judge_timeout_secs: 300,
            retry_attempts: 1,
            max_concurrency: 6,
            daily_cap: 500,
        }
    }
}

/// `[memory]` section.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryConfig {
    /// Master switch; `--no-memory` flips it off.
    pub enabled: bool,
    /// Read entries but never write (`--memory-readonly`, eval arm).
    pub readonly: bool,
    /// Top-k precedents injected per question.
    pub top_k: usize,
    /// Max active rulings injected per question (and the consolidate cap).
    pub max_rulings: usize,
    /// Total memory block size cap, chars.
    pub memory_char_cap: usize,
    /// Keep the last juror memory-blind as an independent control vote.
    pub blind_juror: bool,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            readonly: false,
            top_k: 3,
            max_rulings: 8,
            memory_char_cap: 4000,
            blind_juror: false,
        }
    }
}

/// Fully-resolved runtime configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// `"<backend>:<model>[@<effort>]"` juror strings.
    pub jurors: Vec<String>,
    /// Ballots per juror.
    pub samples: u32,
    /// Judge model string.
    pub judge: String,
    /// Hung-jury policy.
    pub escalate: Escalate,
    /// Confidence below this ⇒ the question is hung.
    pub hung_threshold: f64,
    /// `--explain`: jurors add a short `_why` per answer.
    pub explain: bool,
    /// `--no-cache`.
    pub no_cache: bool,
    /// `--refresh`: skip cache reads, still write.
    pub refresh: bool,
    /// Prompt-override directory (falls back to embedded templates).
    pub prompts_dir: Option<PathBuf>,
    /// Data dir (memory.db, cache/, calls.jsonl, state.json).
    pub data_dir: PathBuf,
    /// Memory db path (default `<data_dir>/memory.db`).
    pub memory_db: PathBuf,
    /// Limits.
    pub limits: LimitsConfig,
    /// Memory tuning.
    pub memory: MemoryConfig,
    /// Name of the applied profile, if any.
    pub profile: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        let data_dir = default_data_dir();
        Self {
            jurors: Vec::new(),
            samples: 1,
            judge: String::new(),
            escalate: Escalate::Sync,
            hung_threshold: 0.5,
            explain: false,
            no_cache: false,
            refresh: false,
            prompts_dir: None,
            memory_db: data_dir.join("memory.db"),
            data_dir,
            limits: LimitsConfig::default(),
            memory: MemoryConfig::default(),
            profile: None,
        }
    }
}

/// `$HUNGJURY_HOME` else `ProjectDirs` data dir; `.` as a last resort.
fn default_data_dir() -> PathBuf {
    if let Some(h) = std::env::var_os("HUNGJURY_HOME") {
        return PathBuf::from(h);
    }
    directories::ProjectDirs::from("", "", "hungjury")
        .map(|p| p.data_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from(".hungjury"))
}

/// Optional TOML file shape — every field optional, merged over defaults.
/// Also used for `[profiles.<name>]` entries (their `profiles` key is ignored).
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct TomlConfig {
    jurors: Option<Vec<String>>,
    samples: Option<u32>,
    judge: Option<String>,
    escalate: Option<Escalate>,
    hung_threshold: Option<f64>,
    prompts_dir: Option<PathBuf>,
    limits: Option<LimitsPartial>,
    memory: Option<MemoryPartial>,
    /// Named profiles selectable via `--profile <name>`.
    profiles: Option<HashMap<String, TomlConfig>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct LimitsPartial {
    juror_timeout_secs: Option<u64>,
    juror_workspace_timeout_secs: Option<u64>,
    judge_timeout_secs: Option<u64>,
    retry_attempts: Option<u32>,
    max_concurrency: Option<usize>,
    daily_cap: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct MemoryPartial {
    enabled: Option<bool>,
    top_k: Option<usize>,
    max_rulings: Option<usize>,
    memory_char_cap: Option<usize>,
    blind_juror: Option<bool>,
}

/// CLI-supplied overrides (already flattened from clap args).
#[derive(Debug, Default, Clone)]
pub struct CliOverrides {
    /// `--jurors a,b,c`
    pub jurors: Option<Vec<String>>,
    /// `--samples N`
    pub samples: Option<u32>,
    /// `--judge m`
    pub judge: Option<String>,
    /// `--escalate sync|queue|off`
    pub escalate: Option<Escalate>,
    /// `--hung-threshold x`
    pub hung_threshold: Option<f64>,
    /// `--no-memory`
    pub no_memory: bool,
    /// `--memory-readonly`
    pub memory_readonly: bool,
    /// `--explain`
    pub explain: bool,
    /// `--no-cache`
    pub no_cache: bool,
    /// `--refresh`
    pub refresh: bool,
    /// `--prompts-dir`
    pub prompts_dir: Option<PathBuf>,
    /// `--memory-db`
    pub memory_db: Option<PathBuf>,
    /// `--profile`
    pub profile: Option<String>,
}

impl Config {
    /// Merge order: defaults → global config → project `hungjury.toml` →
    /// profile → CLI flags → PATH auto-detection for jurors/judge nobody set.
    pub fn load(cli: &CliOverrides, config_path: Option<&Path>) -> Result<Config> {
        let mut cfg = Config::default();
        // [jurors, judge] — explicitly configured somewhere?
        let mut models_set = [false; 2];

        // Global user config.
        let global_toml = global_config_path().map(|p| load_toml(&p)).transpose()?;
        if let Some(t) = &global_toml {
            track_models(t, &mut models_set);
            cfg.apply_toml(t);
        }

        // Project TOML layer.
        let toml_path = config_path
            .map(|p| p.to_path_buf())
            .or_else(|| PathBuf::from("hungjury.toml").is_file().then_some(PathBuf::from("hungjury.toml")));
        let project_toml = toml_path.map(|p| load_toml(&p)).transpose()?;
        if let Some(t) = &project_toml {
            track_models(t, &mut models_set);
            cfg.apply_toml(t);
        }

        // Profile layer — project profiles shadow global ones, and both
        // shadow the built-ins: `default` (a no-op) and bare backend names.
        if let Some(name) = cli.profile.as_deref() {
            if let Some(profile) = find_profile(name, project_toml.as_ref(), global_toml.as_ref()) {
                track_models(profile, &mut models_set);
                cfg.apply_toml(profile);
            } else if let Some(builtin) = builtin_profile(name) {
                track_models(&builtin, &mut models_set);
                cfg.apply_toml(&builtin);
            } else {
                return Err(unknown_profile(
                    name,
                    project_toml.as_ref(),
                    global_toml.as_ref(),
                ));
            }
            cfg.profile = Some(name.to_string());
        }

        // CLI layer.
        if let Some(j) = &cli.jurors {
            cfg.jurors = j.clone();
            models_set[0] = true;
        }
        if let Some(n) = cli.samples {
            cfg.samples = n.max(1);
        }
        if let Some(m) = &cli.judge {
            cfg.judge = m.clone();
            models_set[1] = true;
        }
        if let Some(e) = cli.escalate {
            cfg.escalate = e;
        }
        if let Some(t) = cli.hung_threshold {
            cfg.hung_threshold = t;
        }
        if cli.no_memory {
            cfg.memory.enabled = false;
        }
        if cli.memory_readonly {
            cfg.memory.readonly = true;
        }
        cfg.explain |= cli.explain;
        cfg.no_cache |= cli.no_cache;
        cfg.refresh |= cli.refresh;
        if let Some(p) = &cli.prompts_dir {
            cfg.prompts_dir = Some(p.clone());
        }
        if let Some(p) = &cli.memory_db {
            cfg.memory_db = p.clone();
        }

        // Auto-detect: each CLI on PATH contributes its default juror; the
        // judge falls to the first CLI in claude → codex → devin order.
        if !models_set[0] && cfg.jurors.is_empty() {
            cfg.jurors = BackendKind::detect_all()
                .iter()
                .map(|k| k.default_models().0)
                .collect();
        }
        if !models_set[1] && cfg.judge.is_empty()
            && let Some(kind) = BackendKind::detect()
        {
            cfg.judge = kind.default_models().1;
        }

        // Validate model strings early — a typo should fail before spawn.
        for m in cfg.jurors.iter().chain(std::iter::once(&cfg.judge)) {
            if m.is_empty() {
                continue;
            }
            BackendKind::parse(m)?;
        }
        if cfg.jurors.is_empty() {
            return Err(Error::Config(
                "no jurors configured and no agent CLI found on PATH".to_string(),
            ));
        }
        if cfg.judge.is_empty() {
            return Err(Error::Config(
                "no judge configured and no agent CLI found on PATH".to_string(),
            ));
        }
        Ok(cfg)
    }

    fn apply_toml(&mut self, t: &TomlConfig) {
        if let Some(v) = &t.jurors {
            self.jurors = v.clone();
        }
        if let Some(v) = t.samples {
            self.samples = v.max(1);
        }
        if let Some(v) = &t.judge {
            self.judge = v.clone();
        }
        if let Some(v) = t.escalate {
            self.escalate = v;
        }
        if let Some(v) = t.hung_threshold {
            self.hung_threshold = v;
        }
        if let Some(v) = &t.prompts_dir {
            self.prompts_dir = Some(v.clone());
        }
        if let Some(l) = &t.limits {
            if let Some(v) = l.juror_timeout_secs {
                self.limits.juror_timeout_secs = v;
            }
            if let Some(v) = l.juror_workspace_timeout_secs {
                self.limits.juror_workspace_timeout_secs = v;
            }
            if let Some(v) = l.judge_timeout_secs {
                self.limits.judge_timeout_secs = v;
            }
            if let Some(v) = l.retry_attempts {
                self.limits.retry_attempts = v;
            }
            if let Some(v) = l.max_concurrency {
                self.limits.max_concurrency = v.max(1);
            }
            if let Some(v) = l.daily_cap {
                self.limits.daily_cap = v;
            }
        }
        if let Some(m) = &t.memory {
            if let Some(v) = m.enabled {
                self.memory.enabled = v;
            }
            if let Some(v) = m.top_k {
                self.memory.top_k = v;
            }
            if let Some(v) = m.max_rulings {
                self.memory.max_rulings = v;
            }
            if let Some(v) = m.memory_char_cap {
                self.memory.memory_char_cap = v;
            }
            if let Some(v) = m.blind_juror {
                self.memory.blind_juror = v;
            }
        }
    }

    /// `~/.config/hungjury/config.toml` (or `$XDG_CONFIG_HOME/…`).
    #[allow(dead_code)]
    pub fn global_config_file() -> Option<PathBuf> {
        global_config_path()
    }

    /// Per-juror timeout; workspace states get the longer budget.
    pub fn juror_timeout(&self, workspace: bool) -> Duration {
        Duration::from_secs(if workspace {
            self.limits.juror_workspace_timeout_secs
        } else {
            self.limits.juror_timeout_secs
        })
    }

    /// Judge call timeout.
    pub fn judge_timeout(&self) -> Duration {
        Duration::from_secs(self.limits.judge_timeout_secs)
    }
}

/// Locate the global user config: `$XDG_CONFIG_HOME/hungjury/config.toml`,
/// falling back to `~/.config/hungjury/config.toml`. `None` when absent.
fn global_config_path() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        candidates.push(PathBuf::from(xdg).join("hungjury/config.toml"));
    }
    if let Some(home) = std::env::home_dir() {
        candidates.push(home.join(".config/hungjury/config.toml"));
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// Record whether a TOML layer sets jurors/judge explicitly — slots left
/// unset everywhere become eligible for PATH auto-detection.
fn track_models(t: &TomlConfig, set: &mut [bool; 2]) {
    set[0] |= t.jurors.is_some();
    set[1] |= t.judge.is_some();
}

/// Find profile `name` in TOML (project shadows global). `None` means the
/// name may still resolve to a built-in — see [`builtin_profile`].
fn find_profile<'a>(
    name: &str,
    project: Option<&'a TomlConfig>,
    global: Option<&'a TomlConfig>,
) -> Option<&'a TomlConfig> {
    project
        .and_then(|t| t.profiles.as_ref()?.get(name))
        .or_else(|| global.and_then(|t| t.profiles.as_ref()?.get(name)))
}

/// Built-in profiles, shadowed by any TOML profile of the same name:
/// `default` is a no-op layer; bare backend names (`devin`, `claude`,
/// `codex`) select that CLI's default (juror, judge) pair.
fn builtin_profile(name: &str) -> Option<TomlConfig> {
    if name == "default" {
        return Some(TomlConfig::default());
    }
    let (kind, _) = BackendKind::parse(name).ok()?;
    if kind == BackendKind::Mock {
        return None;
    }
    let (juror, judge) = kind.default_models();
    Some(TomlConfig {
        jurors: Some(vec![juror]),
        judge: Some(judge),
        ..Default::default()
    })
}

/// `unknown profile` error listing the built-ins plus every TOML-defined
/// profile name.
fn unknown_profile(name: &str, project: Option<&TomlConfig>, global: Option<&TomlConfig>) -> Error {
    let mut avail: Vec<String> = ["default", "devin", "claude", "codex"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    for t in [project, global].into_iter().flatten() {
        if let Some(p) = &t.profiles {
            avail.extend(p.keys().cloned());
        }
    }
    avail.sort();
    avail.dedup();
    Error::Config(format!(
        "unknown profile '{name}' (available: {})",
        avail.join(", ")
    ))
}

/// Read + parse a TOML config file.
fn load_toml(path: &Path) -> Result<TomlConfig> {
    let text = std::fs::read_to_string(path).map_err(|e| Error::io(path, e))?;
    toml::from_str(&text).map_err(|e| Error::Config(format!("{}: {e}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toml_then_cli_precedence() {
        let dir = tempfile::tempdir().unwrap();
        let toml_path = dir.path().join("hungjury.toml");
        std::fs::write(
            &toml_path,
            r#"
jurors = ["mock:a", "mock:b"]
judge = "mock:j"
samples = 3
hung_threshold = 0.7
[limits]
daily_cap = 5
[memory]
top_k = 1
"#,
        )
        .unwrap();
        let cli = CliOverrides {
            samples: Some(9),
            ..Default::default()
        };
        let cfg = Config::load(&cli, Some(&toml_path)).unwrap();
        assert_eq!(cfg.samples, 9); // CLI wins
        assert_eq!(cfg.jurors, vec!["mock:a", "mock:b"]);
        assert_eq!(cfg.judge, "mock:j");
        assert!((cfg.hung_threshold - 0.7).abs() < 1e-9);
        assert_eq!(cfg.limits.daily_cap, 5);
        assert_eq!(cfg.memory.top_k, 1);
    }

    #[test]
    fn explicit_mock_models_skip_autodetect() {
        let cli = CliOverrides {
            jurors: Some(vec!["mock:a".to_string()]),
            judge: Some("mock:j".to_string()),
            ..Default::default()
        };
        let cfg = Config::load(&cli, None).unwrap();
        assert_eq!(cfg.jurors, vec!["mock:a"]);
        assert_eq!(cfg.judge, "mock:j");
    }

    #[test]
    fn backend_names_are_builtin_profiles() {
        let p = builtin_profile("claude").unwrap();
        assert_eq!(p.jurors.unwrap(), vec!["claude:haiku"]);
        assert_eq!(p.judge.as_deref(), Some("claude:opus@high"));
        assert!(builtin_profile("devin").is_some());
        assert!(builtin_profile("codex").is_some());
        // `mock`/`test` parse as a backend but are not profiles.
        assert!(builtin_profile("mock").is_none());
        assert!(builtin_profile("test").is_none());
    }

    #[test]
    fn unknown_profile_errors_with_available_list() {
        let dir = tempfile::tempdir().unwrap();
        let toml_path = dir.path().join("hungjury.toml");
        std::fs::write(&toml_path, "[profiles.default]\nsamples = 2\n").unwrap();
        let cli = CliOverrides {
            profile: Some("nope".to_string()),
            jurors: Some(vec!["mock:a".to_string()]),
            judge: Some("mock:j".to_string()),
            ..Default::default()
        };
        let err = Config::load(&cli, Some(&toml_path)).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("unknown profile 'nope'"), "{msg}");
        for b in ["default", "devin", "claude", "codex"] {
            assert!(msg.contains(b), "{msg}");
        }
    }

    #[test]
    fn memory_db_flag_overrides() {
        let cli = CliOverrides {
            jurors: Some(vec!["mock:a".to_string()]),
            judge: Some("mock:j".to_string()),
            memory_db: Some(PathBuf::from("/tmp/x.db")),
            ..Default::default()
        };
        let cfg = Config::load(&cli, None).unwrap();
        assert_eq!(cfg.memory_db, PathBuf::from("/tmp/x.db"));
    }
}
