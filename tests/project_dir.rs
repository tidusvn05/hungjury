//! `.hungjury/` project-dir discovery, init, and namespace isolation.
//!
//! `Config::load_at` takes an explicit start dir so these tests don't
//! need to change process cwd. `HUNGJURY_HOME` is cleared where the
//! global fallback matters.

use std::path::Path;
use std::process::Command;

use hungjury::config::{CliOverrides, Config, discover_project};
use hungjury::memory::store::{Kind, NewEntry, Source, Store, q_scope};

fn hj_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_hungjury"))
}

/// Overrides with mock jurors/judge — `Config::load*` scans PATH for
/// agent CLIs when they are unset, which fails where none exist (CI).
fn mock_over() -> CliOverrides {
    CliOverrides {
        jurors: Some(vec!["mock:x".into()]),
        judge: Some("mock:j".into()),
        ..CliOverrides::default()
    }
}

fn write(p: &Path, s: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, s).unwrap();
}

/// Serializes env-var mutation across parallel tests.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn without_home(f: impl FnOnce()) {
    let _g = ENV_LOCK.lock().unwrap();
    let prev = std::env::var_os("HUNGJURY_HOME");
    unsafe { std::env::remove_var("HUNGJURY_HOME") };
    f();
    unsafe {
        match prev {
            Some(v) => std::env::set_var("HUNGJURY_HOME", v),
            None => std::env::remove_var("HUNGJURY_HOME"),
        }
    }
}

fn with_home(home: &Path, f: impl FnOnce()) {
    let _g = ENV_LOCK.lock().unwrap();
    let prev = std::env::var_os("HUNGJURY_HOME");
    unsafe { std::env::set_var("HUNGJURY_HOME", home) };
    f();
    unsafe {
        match prev {
            Some(v) => std::env::set_var("HUNGJURY_HOME", v),
            None => std::env::remove_var("HUNGJURY_HOME"),
        }
    }
}

// ---------- discovery ----------

#[test]
fn walkup_finds_hungjury_dir_and_toml() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    let nested = root.join("a/b/c");
    std::fs::create_dir_all(root.join(".hungjury")).unwrap();
    std::fs::create_dir_all(&nested).unwrap();
    write(&root.join("hungjury.toml"), "judge = \"mock:j\"\n");

    let found = discover_project(&nested);
    assert_eq!(
        found.hungjury_dir.as_deref(),
        Some(root.join(".hungjury").as_path())
    );
    assert_eq!(
        found.toml.as_deref(),
        Some(root.join("hungjury.toml").as_path())
    );
}

#[test]
fn hungjury_config_toml_preferred_over_flat() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    write(
        &root.join(".hungjury/config.toml"),
        "judge = \"mock:nested\"\n",
    );
    write(&root.join("hungjury.toml"), "judge = \"mock:flat\"\n");

    let found = discover_project(&root);
    assert!(
        found
            .toml
            .as_deref()
            .unwrap()
            .ends_with(".hungjury/config.toml")
    );
}

#[test]
fn no_project_means_global() {
    let dir = tempfile::tempdir().unwrap();
    let found = discover_project(dir.path());
    assert!(found.hungjury_dir.is_none());
    assert!(found.toml.is_none());
}

// ---------- Config::load_at ----------

#[test]
fn project_dir_becomes_data_dir() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    let nested = root.join("sub/dir");
    std::fs::create_dir_all(root.join(".hungjury")).unwrap();
    std::fs::create_dir_all(&nested).unwrap();

    // HUNGJURY_HOME must not interfere with this assertion.
    without_home(|| {
        let cfg = Config::load_at(&mock_over(), None, &nested).unwrap();
        assert_eq!(cfg.data_dir, root.join(".hungjury"));
        assert_eq!(cfg.memory_db, root.join(".hungjury/memory.db"));
        assert_eq!(cfg.project_root.as_deref(), Some(root.as_path()));
    });
}

#[test]
fn hungjury_home_beats_project_dir() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    std::fs::create_dir_all(root.join(".hungjury")).unwrap();
    let home = dir.path().join("global_home");

    with_home(&home, || {
        let cfg = Config::load_at(&mock_over(), None, &root).unwrap();
        assert_eq!(cfg.data_dir, home);
    });
}

#[test]
fn memory_db_flag_wins_everything() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    std::fs::create_dir_all(root.join(".hungjury")).unwrap();
    let custom = dir.path().join("custom.db");

    let over = CliOverrides {
        memory_db: Some(custom.clone()),
        ..mock_over()
    };
    let cfg = Config::load_at(&over, None, &root).unwrap();
    assert_eq!(cfg.memory_db, custom);
}

#[test]
fn toml_paths_resolve_against_toml_dir() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    write(
        &root.join(".hungjury/config.toml"),
        "policy_file = \"rules/pol.md\"\nmemory_db = \"mem.db\"\n",
    );
    write(&root.join(".hungjury/rules/pol.md"), "# rules\n");

    let nested = root.join("deep/nest");
    std::fs::create_dir_all(&nested).unwrap();
    let cfg = Config::load_at(&mock_over(), None, &nested).unwrap();
    assert_eq!(
        cfg.policy_file.as_deref(),
        Some(root.join(".hungjury/rules/pol.md").as_path())
    );
    assert_eq!(cfg.memory_db, root.join(".hungjury/mem.db"));
    assert!(cfg.policy.is_some());
}

#[test]
fn policy_md_auto_detected() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    write(&root.join(".hungjury/policy.md"), "# auto policy\n");

    let cfg = Config::load_at(&mock_over(), None, &root).unwrap();
    assert_eq!(
        cfg.policy_file.as_deref(),
        Some(root.join(".hungjury/policy.md").as_path())
    );
    assert_eq!(cfg.policy.as_deref(), Some("# auto policy"));
}

#[test]
fn bare_toml_loads_but_memory_stays_global() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    write(&root.join("hungjury.toml"), "min_quorum = 3\n");

    without_home(|| {
        let cfg = Config::load_at(&mock_over(), None, &root).unwrap();
        assert_eq!(cfg.min_quorum, 3);
        assert_eq!(cfg.project_root.as_deref(), Some(root.as_path()));
        // No .hungjury/ ⇒ memory stays on the global data dir.
        assert!(!cfg.data_dir.starts_with(&root));
    });
}

#[test]
fn profile_memory_db_resolves_in_project() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    write(
        &root.join(".hungjury/config.toml"),
        "[profiles.review]\nmemory_db = \"memory-review.db\"\njurors = [\"mock:x\"]\n",
    );
    let over = CliOverrides {
        profile: Some("review".to_string()),
        ..mock_over()
    };
    let cfg = Config::load_at(&over, None, &root).unwrap();
    assert_eq!(cfg.memory_db, root.join(".hungjury/memory-review.db"));
}

// ---------- namespace ----------

#[test]
fn namespace_partitions_scopes() {
    let store = Store::open_memory().unwrap();
    for (ns, text) in [(None, "global rule"), (Some("triage"), "triage rule")] {
        let e = NewEntry {
            kind: Kind::Ruling,
            scope: q_scope(ns, "q1"),
            body: serde_json::json!({"text": text}),
            text: text.to_string(),
            source: Source::Judge,
            trust: 0.8,
            author: None,
            origin: None,
        };
        store.insert(&e).unwrap();
    }
    let global = store.rulings(None, "q1", 10).unwrap();
    let triage = store.rulings(Some("triage"), "q1", 10).unwrap();
    assert_eq!(global.len(), 1);
    assert_eq!(global[0].text, "global rule");
    assert_eq!(triage.len(), 1);
    assert_eq!(triage[0].text, "triage rule");
    assert_eq!(triage[0].scope, "triage:q:q1");

    let ns = store.namespaces().unwrap();
    assert_eq!(ns.len(), 2);
    assert_eq!(ns[0], ("".to_string(), 1));
    assert_eq!(ns[1], ("triage".to_string(), 1));
}

// ---------- init (real binary) ----------

#[test]
fn init_scaffolds_and_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let out = hj_bin().arg("init").arg(dir.path()).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    for f in ["config.toml", "policy.md", ".gitignore"] {
        assert!(dir.path().join(".hungjury").join(f).is_file(), "{f}");
    }
    let gi = std::fs::read_to_string(dir.path().join(".hungjury/.gitignore")).unwrap();
    assert!(gi.contains("memory*.db"));

    // Second run keeps existing files.
    std::fs::write(dir.path().join(".hungjury/policy.md"), "# mine\n").unwrap();
    let out = hj_bin().arg("init").arg(dir.path()).output().unwrap();
    assert!(out.status.success());
    assert_eq!(
        std::fs::read_to_string(dir.path().join(".hungjury/policy.md")).unwrap(),
        "# mine\n"
    );
}
