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
    /// Trust given to rulings distilled from a hung-key escalation —
    /// provisional until a later judge/human re-confirms them.
    pub provisional_trust: f64,
    /// Age in days after which an active ruling goes `stale` when `learn`
    /// runs. `0` disables expiry.
    pub ruling_ttl_days: u32,
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
            provisional_trust: 0.4,
            ruling_ttl_days: 0,
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
    /// Fewer valid ballots than this ⇒ hung (a lone surviving juror must
    /// not silently decide). Default 2.
    pub min_quorum: usize,
    /// `--explain`: jurors add a short `_why` per answer.
    pub explain: bool,
    /// `--no-cache`.
    pub no_cache: bool,
    /// `--refresh`: skip cache reads, still write.
    pub refresh: bool,
    /// Prompt-override directory (falls back to embedded templates).
    pub prompts_dir: Option<PathBuf>,
    /// `--policy-file` path (TOML `policy_file`); domain rules injected
    /// into juror + judge prompts.
    pub policy_file: Option<PathBuf>,
    /// Resolved policy text (loaded from `policy_file` once at `load`).
    #[serde(skip_serializing)]
    pub policy: Option<String>,
    /// Data dir (memory.db, cache/, calls.jsonl, state.json).
    pub data_dir: PathBuf,
    /// Memory db path (default `<data_dir>/memory.db`).
    pub memory_db: PathBuf,
    /// Estimated USD per CLI call, keyed by backend name
    /// (`claude`/`codex`/`devin`). Empty ⇒ no cost estimate.
    pub costs: std::collections::BTreeMap<String, f64>,
    /// Limits.
    pub limits: LimitsConfig,
    /// Memory tuning.
    pub memory: MemoryConfig,
    /// Name of the applied profile, if any.
    pub profile: Option<String>,
    /// Soft memory partition — scope prefix (`ns:q:<qid>`). `None`
    /// keeps today's `q:`/`ws:` scopes.
    pub namespace: Option<String>,
    /// Directory containing the discovered `.hungjury/` (or walked
    /// `hungjury.toml`). `None` when running without a project.
    pub project_root: Option<PathBuf>,
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
            min_quorum: 2,
            explain: false,
            no_cache: false,
            refresh: false,
            prompts_dir: None,
            policy_file: None,
            policy: None,
            costs: std::collections::BTreeMap::new(),
            memory_db: data_dir.join("memory.db"),
            data_dir,
            limits: LimitsConfig::default(),
            memory: MemoryConfig::default(),
            profile: None,
            namespace: None,
            project_root: None,
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

/// What a walk-up from the working directory found.
#[derive(Debug, Default, Clone)]
pub struct ProjectDir {
    /// Nearest ancestor containing `.hungjury/` — its parent is the
    /// project root and the dir itself becomes `data_dir`.
    pub hungjury_dir: Option<PathBuf>,
    /// Nearest `.hungjury/config.toml` or `hungjury.toml` — the project
    /// config layer (may sit at a different level than `hungjury_dir`).
    pub toml: Option<PathBuf>,
}

/// Walk ancestors of `start` for `.hungjury/` and `hungjury.toml` —
/// git-style: the nearest hit wins each, independently.
pub fn discover_project(start: &Path) -> ProjectDir {
    let mut found = ProjectDir::default();
    for dir in start.ancestors() {
        if found.hungjury_dir.is_none() {
            let d = dir.join(".hungjury");
            if d.is_dir() {
                found.hungjury_dir = Some(d);
            }
        }
        if found.toml.is_none() {
            let nested = dir.join(".hungjury/config.toml");
            let flat = dir.join("hungjury.toml");
            found.toml = if nested.is_file() {
                Some(nested)
            } else if flat.is_file() {
                Some(flat)
            } else {
                None
            };
        }
        if found.hungjury_dir.is_some() && found.toml.is_some() {
            break;
        }
    }
    found
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
    min_quorum: Option<usize>,
    prompts_dir: Option<PathBuf>,
    policy_file: Option<PathBuf>,
    /// Soft memory partition (scope prefix `ns:q:<qid>`).
    namespace: Option<String>,
    /// Per-purpose db path — relative paths resolve against the
    /// directory holding the toml that declared them.
    memory_db: Option<PathBuf>,
    /// `[costs]` — backend name → USD per call.
    costs: Option<std::collections::BTreeMap<String, f64>>,
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
    provisional_trust: Option<f64>,
    ruling_ttl_days: Option<u32>,
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
    /// `--min-quorum n`
    pub min_quorum: Option<usize>,
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
    /// `--policy-file`
    pub policy_file: Option<PathBuf>,
    /// `--memory-db`
    pub memory_db: Option<PathBuf>,
    /// `--profile`
    pub profile: Option<String>,
    /// `--namespace`
    pub namespace: Option<String>,
}

impl Config {
    /// Merge order: defaults → global config → project `hungjury.toml` →
    /// profile → CLI flags → PATH auto-detection for jurors/judge nobody set.
    pub fn load(cli: &CliOverrides, config_path: Option<&Path>) -> Result<Config> {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self::load_at(cli, config_path, &cwd)
    }

    /// `load` with an explicit working directory — tests + callers that
    /// already resolved their cwd.
    pub fn load_at(cli: &CliOverrides, config_path: Option<&Path>, start: &Path) -> Result<Config> {
        // Walk-up discovery: `.hungjury/` anchors project-local state;
        // `hungjury.toml`/`.hungjury/config.toml` is the project config
        // layer. Either may sit at any ancestor — nearest wins.
        let proj = discover_project(start);
        let mut cfg = Config::default();
        if std::env::var_os("HUNGJURY_HOME").is_none()
            && let Some(d) = &proj.hungjury_dir
        {
            cfg.data_dir = d.clone();
            cfg.memory_db = d.join("memory.db");
        }
        cfg.project_root = proj
            .hungjury_dir
            .as_deref()
            .and_then(|p| p.parent())
            .or_else(|| proj.toml.as_deref().and_then(|p| p.parent()))
            .map(Path::to_path_buf);
        // [jurors, judge] — explicitly configured somewhere?
        let mut models_set = [false; 2];

        // Global user config.
        let global_path = global_config_path();
        let global_toml = global_path.as_ref().map(|p| load_toml(p)).transpose()?;
        if let Some(t) = &global_toml {
            track_models(t, &mut models_set);
            cfg.apply_toml(t, global_path.as_deref().and_then(|p| p.parent()));
        }

        // Project TOML layer: --config > discovered (config.toml inside
        // .hungjury/ preferred over a flat hungjury.toml).
        let toml_path = config_path.map(|p| p.to_path_buf()).or(proj.toml.clone());
        let project_toml = toml_path.as_ref().map(|p| load_toml(p)).transpose()?;
        if let Some(t) = &project_toml {
            track_models(t, &mut models_set);
            cfg.apply_toml(t, toml_path.as_deref().and_then(|p| p.parent()));
        }

        // Profile layer — project profiles shadow global ones, and both
        // shadow the built-ins: `default` (a no-op) and bare backend names.
        if let Some(name) = cli.profile.as_deref() {
            if let Some((profile, from_project)) =
                find_profile(name, project_toml.as_ref(), global_toml.as_ref())
            {
                track_models(profile, &mut models_set);
                let base = if from_project {
                    toml_path.as_deref().and_then(|p| p.parent())
                } else {
                    global_path.as_deref().and_then(|p| p.parent())
                };
                cfg.apply_toml(profile, base);
            } else if let Some(builtin) = builtin_profile(name) {
                track_models(&builtin, &mut models_set);
                cfg.apply_toml(&builtin, None);
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
        if let Some(q) = cli.min_quorum {
            cfg.min_quorum = q.max(1);
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
        if let Some(p) = &cli.policy_file {
            cfg.policy_file = Some(p.clone());
        }
        if let Some(p) = &cli.memory_db {
            cfg.memory_db = p.clone();
        }
        if let Some(ns) = &cli.namespace {
            cfg.namespace = Some(ns.clone()).filter(|s| !s.is_empty());
        }

        // `.hungjury/` conventions: a policy.md or prompts/ dir found by
        // walk-up apply automatically unless something already set them.
        if let Some(d) = &proj.hungjury_dir {
            if cfg.policy_file.is_none() {
                let p = d.join("policy.md");
                if p.is_file() {
                    cfg.policy_file = Some(p);
                }
            }
            if cfg.prompts_dir.is_none() {
                let p = d.join("prompts");
                if p.is_dir() {
                    cfg.prompts_dir = Some(p);
                }
            }
        }

        // Resolve the domain policy text once — injected into juror and
        // judge prompts as `{{policy_block}}`.
        if let Some(p) = &cfg.policy_file {
            let text = std::fs::read_to_string(p).map_err(|e| Error::io(p, e))?;
            let text = text.trim().to_string();
            cfg.policy = (!text.is_empty()).then_some(text);
        }

        // Auto-detect: each CLI on PATH contributes its default juror; the
        // judge falls to the first CLI in claude → codex → devin order.
        if !models_set[0] && cfg.jurors.is_empty() {
            cfg.jurors = BackendKind::detect_all()
                .iter()
                .map(|k| k.default_models().0)
                .collect();
        }
        if !models_set[1]
            && cfg.judge.is_empty()
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

    /// Merge one toml layer. `base` is the directory holding the toml —
    /// relative `prompts_dir`/`policy_file`/`memory_db` resolve against
    /// it, so a config found by walk-up works from any cwd.
    fn apply_toml(&mut self, t: &TomlConfig, base: Option<&Path>) {
        let rel = |p: &PathBuf| -> PathBuf {
            match (p.is_absolute(), base) {
                (true, _) | (_, None) => p.clone(),
                (false, Some(b)) => b.join(p),
            }
        };
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
        if let Some(v) = t.min_quorum {
            self.min_quorum = v.max(1);
        }
        if let Some(v) = &t.costs {
            self.costs = v.clone();
        }
        if let Some(v) = &t.prompts_dir {
            self.prompts_dir = Some(rel(v));
        }
        if let Some(v) = &t.policy_file {
            self.policy_file = Some(rel(v));
        }
        if let Some(v) = &t.memory_db {
            self.memory_db = rel(v);
        }
        if let Some(v) = &t.namespace {
            self.namespace = Some(v.clone()).filter(|s| !s.is_empty());
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
            if let Some(v) = m.provisional_trust {
                self.memory.provisional_trust = v.clamp(0.0, 1.0);
            }
            if let Some(v) = m.ruling_ttl_days {
                self.memory.ruling_ttl_days = v;
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

    /// Configured USD price of one call to this agent string's backend —
    /// `None` when `[costs]` has no entry for it.
    pub fn cost_per_call(&self, agent: &str) -> Option<f64> {
        let backend = agent.split(':').next().unwrap_or(agent);
        self.costs.get(backend).copied()
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
/// `(profile, base_dir)` — base_dir is the dir of the toml the profile
/// came from, so its relative paths resolve correctly.
fn find_profile<'a>(
    name: &str,
    project: Option<&'a TomlConfig>,
    global: Option<&'a TomlConfig>,
) -> Option<(&'a TomlConfig, bool)> {
    if let Some(t) = project.and_then(|t| t.profiles.as_ref()?.get(name)) {
        return Some((t, true));
    }
    global
        .and_then(|t| t.profiles.as_ref()?.get(name))
        .map(|t| (t, false))
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
    fn toml_quorum_costs_and_memory_tuning() {
        let dir = tempfile::tempdir().unwrap();
        let toml_path = dir.path().join("hungjury.toml");
        std::fs::write(
            &toml_path,
            r#"
jurors = ["mock:a", "mock:b"]
judge = "mock:j"
min_quorum = 3
[costs]
claude = 0.08
codex = 0.05
[memory]
provisional_trust = 0.3
ruling_ttl_days = 90
"#,
        )
        .unwrap();
        let cfg = Config::load(&CliOverrides::default(), Some(&toml_path)).unwrap();
        assert_eq!(cfg.min_quorum, 3);
        assert_eq!(cfg.cost_per_call("claude:opus@high"), Some(0.08));
        assert_eq!(cfg.cost_per_call("devin:x"), None);
        assert!((cfg.memory.provisional_trust - 0.3).abs() < 1e-9);
        assert_eq!(cfg.memory.ruling_ttl_days, 90);
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
