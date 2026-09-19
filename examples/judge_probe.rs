//! Probe: are `judge_call` futures serial under buffer_unordered?
//! Run: `cargo run --example judge_probe` — spends ~4 opus calls.

use std::collections::BTreeMap;
use std::time::Instant;

use futures_util::stream::{self, StreamExt};
use hungjury::config::{CliOverrides, Config};
use hungjury::judge;
use hungjury::jury::DecideCtx;
use hungjury::request::{Request, State};

#[tokio::main]
async fn main() {
    let over = CliOverrides {
        no_cache: true,
        judge: Some("claude:opus@high".into()),
        ..CliOverrides::default()
    };
    let cfg = Config::load(&over, None).unwrap();
    let ctx = DecideCtx::new(cfg, None).unwrap();

    let mk = || {
        Request::from_parts(
            State::Text("charged twice and app crashes".into()),
            r#"{"ok":{"type":"noul","id":"q.ok","instructions":"is this a bug report"}}"#,
        )
        .unwrap()
    };
    let cases: Vec<Request> = (0..4).map(|_| mk()).collect();
    let hung = vec!["ok".to_string()];

    let t0 = Instant::now();
    let results: Vec<_> = stream::iter(
        cases.iter().enumerate().map(|(i, c)| {
            let ctx = &ctx;
            let hung = hung.clone();
            async move {
                let (call, usage) = judge::judge_call(
                    ctx,
                    c,
                    "",
                    &[],
                    &BTreeMap::new(),
                    &hung,
                    &hungjury::util::nonce(),
                    None,
                )
                .await;
                (i, call.is_some(), usage.ms)
            }
        }),
    )
    .buffer_unordered(4)
    .collect()
    .await;
    println!("wall: {:?}", t0.elapsed());
    for r in results {
        println!("{r:?}");
    }
}
