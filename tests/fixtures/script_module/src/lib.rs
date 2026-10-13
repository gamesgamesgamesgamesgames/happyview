//! A script module for HappyView's own tests: one handler per behaviour the
//! host has to get right with real compiled code.

use happyview_script::{export, join, json, query, record_index, Ctx, Result, Value};
use serde::Deserialize;

#[derive(Deserialize)]
struct EchoParams {
    q: String,
}

#[query("app.test.echo")]
async fn echo(input: EchoParams, ctx: Ctx) -> Result<Value> {
    Ok(json!({ "q": input.q, "trigger": ctx.trigger(), "caller": ctx.caller_did() }))
}

/// Two library calls awaited together, so the test can see them overlap.
#[query("app.test.pair")]
async fn pair(input: Value, ctx: Ctx) -> Result<Value> {
    let url = input["url"].clone();
    let (a, b) = join!(
        ctx.call("http", "get", vec![url.clone()]),
        ctx.call("http", "get", vec![url]),
    );
    let (a, b) = (a?, b?);
    Ok(json!([a, b]))
}

#[query("app.test.boom")]
async fn boom(_input: Value, _ctx: Ctx) -> Result<Value> {
    panic!("the fixture panicked on purpose")
}

#[query("app.test.spin")]
async fn spin(_input: Value, _ctx: Ctx) -> Result<Value> {
    let mut n: u64 = 0;
    loop {
        n = std::hint::black_box(n.wrapping_add(1));
        if n == 0 {
            return Ok(json!(n));
        }
    }
}

#[record_index("app.test.thing")]
async fn index_thing(_input: Value, _ctx: Ctx) -> Result<()> {
    Ok(())
}

export!(echo, pair, boom, spin, index_thing);
