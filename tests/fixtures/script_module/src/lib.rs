//! A script module for HappyView's own tests: one handler per behaviour the
//! host has to get right with real compiled code.

use happyview_script::{export, join, json, query, record_index, Ctx, Result, Value};
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

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

// The three calibration handlers do step for step what
// `tests/fixture_scripts/calibration/*.lua` do, so one instruction budget can
// be compared across the two.

#[derive(Deserialize)]
struct Records {
    records: Vec<Envelope>,
}

#[derive(Deserialize)]
struct Envelope {
    uri: String,
    did: String,
    record: Post,
}

#[derive(Deserialize)]
struct Post {
    text: String,
    #[serde(rename = "createdAt")]
    created_at: String,
    tags: Vec<String>,
    #[serde(rename = "likeCount")]
    like_count: i64,
}

#[derive(Serialize)]
struct Item<'a> {
    uri: &'a str,
    author: &'a str,
    text: &'a str,
    tags: &'a [String],
    likes: i64,
}

#[query("app.test.calibration.shape")]
async fn calibration_shape(input: Records, _ctx: Ctx) -> Result<Value> {
    let mut kept: Vec<&Envelope> = input
        .records
        .iter()
        .filter(|r| r.record.like_count >= 5)
        .collect();
    kept.sort_by(|a, b| b.record.created_at.cmp(&a.record.created_at));
    kept.truncate(25);
    let items: Vec<Item> = kept
        .iter()
        .map(|r| Item {
            uri: &r.uri,
            author: &r.did,
            text: &r.record.text,
            tags: &r.record.tags,
            likes: r.record.like_count,
        })
        .collect();
    // No cursor at all when nothing was kept, as a Lua `nil` field is absent.
    let mut out = json!({ "items": items });
    if let Some(last) = kept.last() {
        out["cursor"] = json!(last.record.created_at);
    }
    Ok(out)
}

#[derive(Serialize)]
struct TagCount<'a> {
    tag: &'a str,
    count: i64,
}

/// Tags are ranked from the order they were first seen, as the Lua twin does.
#[query("app.test.calibration.aggregate")]
async fn calibration_aggregate(input: Records, _ctx: Ctx) -> Result<Value> {
    let mut index: BTreeMap<&str, usize> = BTreeMap::new();
    let mut ranked: Vec<TagCount> = Vec::new();
    for r in &input.records {
        for tag in &r.record.tags {
            match index.get(tag.as_str()) {
                Some(&at) => ranked[at].count += 1,
                None => {
                    index.insert(tag, ranked.len());
                    ranked.push(TagCount { tag, count: 1 });
                }
            }
        }
    }
    ranked.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.tag.cmp(b.tag)));
    ranked.truncate(10);
    Ok(json!(ranked))
}

#[query("app.test.calibration.render")]
async fn calibration_render(input: Records, _ctx: Ctx) -> Result<Value> {
    let lines: Vec<String> = input
        .records
        .iter()
        .map(|r| {
            let t = &r.record.tags;
            format!(
                "{} said \"{}\" ({} likes) #{} #{} #{}",
                r.did, r.record.text, r.record.like_count, t[0], t[1], t[2]
            )
        })
        .collect();
    let text = lines.join("\n");
    Ok(json!({ "length": text.len(), "text": text }))
}

export!(
    echo,
    pair,
    boom,
    spin,
    index_thing,
    calibration_shape,
    calibration_aggregate,
    calibration_render,
);
