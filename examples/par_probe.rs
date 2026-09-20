//! Probe: are real-backend `decide` futures serial under buffer_unordered?
//! Run: `cargo run --example par_probe` — spends ~3 claude calls.

use std::time::Instant;

use futures_util::stream::{self, StreamExt};
use hungjury::config::{CliOverrides, Config};
use hungjury::jury::{self, DecideCtx};
use hungjury::request::{Request, State};

#[tokio::main]
async fn main() {
    let over = CliOverrides {
        no_memory: true,
        no_cache: true,
        jurors: Some(vec!["claude:haiku".into()]),
        escalate: Some(hungjury::config::Escalate::Off),
        ..CliOverrides::default()
    };
    let cfg = Config::load(&over, None).unwrap();
    let ctx = DecideCtx::new(cfg, None).unwrap();

    let mk = || {
        Request::from_parts(
            State::Text("The app crashes on checkout".into()),
            r#"{"ok":{"type":"noul","instructions":"is this a bug report"}}"#,
        )
        .unwrap()
    };
    let cases: Vec<Request> = (0..3).map(|_| mk()).collect();

    let t0 = Instant::now();
    let results: Vec<_> = stream::iter(cases.iter().enumerate().map(|(i, c)| {
        let ctx = &ctx;
        async move { (i, jury::decide(ctx, c).await) }
    }))
    .buffer_unordered(3)
    .collect()
    .await;
    println!("wall: {:?}", t0.elapsed());
    for (i, r) in results {
        println!("case {i}: {:?}", r.map(|(resp, _)| resp.usage.wall_ms));
    }
}
