//! Repro: eval-style `buffer_unordered` over `jury::decide` must overlap.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::stream::{self, StreamExt};
use hungjury::backend::mock::MockBackend;
use hungjury::backend::{AgentBackend, BackendKind};
use hungjury::config::{CliOverrides, Config};
use hungjury::jury::{self, DecideCtx};
use hungjury::request::Request;

/// Serializes env-var mutation across parallel tests (same pattern as
/// tests/project_dir.rs).
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn ctx_with_delay(ms: u64) -> DecideCtx {
    let over = CliOverrides {
        no_memory: true,
        no_cache: true,
        jurors: Some(vec!["mock:a".into(), "mock:b".into(), "mock:c".into()]),
        ..CliOverrides::default()
    };
    let cfg = Config::load(&over, None).unwrap();
    let canned = |_name: &'static str| -> Arc<dyn AgentBackend> {
        Arc::new(
            MockBackend::new(move |_req| Ok(r#"{"q1": "x", "q2": true, "q3": 1}"#.to_string()))
                .with_delay(Duration::from_millis(ms)),
        )
    };
    let mut backends: HashMap<BackendKind, Arc<dyn AgentBackend>> = HashMap::new();
    backends.insert(BackendKind::Mock, canned("m"));
    DecideCtx::new(cfg, Some(backends)).unwrap()
}

#[tokio::test]
async fn cases_overlap_under_buffer_unordered() {
    // data_dir (and thus the quota file) resolves inside `ctx_with_delay`
    // — point HUNGJURY_HOME at a throwaway dir so a full real-world
    // quota can't starve the mock jurors.
    let home = tempfile::tempdir().unwrap();
    let ctx = {
        let _g = ENV_LOCK.lock().unwrap();
        let prev = std::env::var_os("HUNGJURY_HOME");
        unsafe { std::env::set_var("HUNGJURY_HOME", home.path()) };
        let ctx = ctx_with_delay(300);
        unsafe {
            match prev {
                Some(v) => std::env::set_var("HUNGJURY_HOME", v),
                None => std::env::remove_var("HUNGJURY_HOME"),
            }
        }
        ctx
    };
    let par = 8usize;
    let cases: Vec<Request> = (0..8)
        .map(|_| {
            Request::from_parts(
                hungjury::request::State::Text("s".into()),
                r#"{"q1":{"type":"choice","instructions":"i","criteria":{"x":"d","y":"d"}},
                   "q2":{"type":"noul","instructions":"i"},
                   "q3":{"type":"score","instructions":"i","criteria":["a","b"]}}"#,
            )
            .unwrap()
        })
        .collect();
    let t0 = Instant::now();
    let results: Vec<_> = stream::iter(cases.iter().enumerate().map(|(i, c)| {
        let ctx = &ctx;
        async move { (i, jury::decide(ctx, c).await) }
    }))
    .buffer_unordered(par)
    .collect()
    .await;
    let wall = t0.elapsed();
    for (i, r) in &results {
        assert!(r.is_ok(), "case {i} failed: {:?}", r.as_ref().err());
    }
    // Serial would be 8 × ~300ms ≈ 2.4s; parallel ≈ ~0.4s.
    assert!(
        wall < Duration::from_millis(1500),
        "cases look serial: wall={wall:?}"
    );
}

#[test]
fn max_concurrency_from_toml() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("hungjury.toml");
    std::fs::write(&p, "[limits]\ndaily_cap = 2000\nmax_concurrency = 8\n").unwrap();
    let over = CliOverrides::default();
    let cfg = Config::load(&over, Some(&p)).unwrap();
    assert_eq!(cfg.limits.max_concurrency, 8);
}
