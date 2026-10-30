//! The library `tests/lua_differential.rs` runs both interpreters against.
//!
//! Two jobs. It **answers** every call the corpus and the bridge cases make,
//! in shapes deep enough that a script indexes into the result and keeps
//! going, so the comparison reaches past the first call. And it **records**
//! each call in its own key-value store, which is what gives the harness a
//! transcript of library calls with their JSON arguments — something nothing
//! else in the host produces.
//!
//! The store rather than `host_log`, which would be the obvious choice: the
//! host writes a log row from a detached task, so a transcript read straight
//! after a run is missing however much of its tail has not landed yet, and no
//! amount of waiting makes that sound — a count that has not started moving
//! reads exactly like one that has finished. A `kv` write is awaited before
//! the call returns, so when a run ends its transcript is already whole.
//!
//! The store is keyed per plugin id, and this fixture is registered once per
//! namespace, so what the harness reads back is one ordered list per library
//! rather than one interleaved list. Order within a library is exact; order
//! between two libraries is not compared.
//!
//! Every answer is a constant. Neither the clock nor the database is
//! involved, because a differential comparison of two runs cannot afford a
//! third thing changing between them.
//!
//! Its surface is the union of the ten standard libraries' surfaces, so one
//! registration serves every namespace a script may require. One name has to
//! be chosen rather than unioned: `get` is a plain function in
//! `happyview.db` and a constructor in `happyview.spaces` and
//! `happyview.linked_repos`, and it is a function here. A script using the
//! other form fails — identically on both paths, which is all this harness
//! needs.

#![cfg_attr(target_arch = "wasm32", no_std)]

extern crate alloc;

use alloc::string::ToString;
use alloc::vec::Vec;

use happyview_plugin_sdk::host;
use happyview_plugin_sdk::{
    ApiExport, ApiSurface, CallContext, PluginError, PluginInfo, Value, json, library_plugin,
};

library_plugin! {
    info: PluginInfo::new("differential_library", "Differential library", "1.0.0"),
    surface: surface,
    call: dispatch,
}

/// Every plain export the ten standard libraries publish.
const FUNCTIONS: [&str; 27] = [
    "accept_invite",
    "backend",
    "blob_download",
    "create",
    "delete",
    "delete_local",
    "get",
    "get_labels",
    "get_labels_batch",
    "head",
    "info",
    "lexicon",
    "list",
    "load",
    "patch",
    "post",
    "procedure",
    "put",
    "query",
    "raw",
    "resolve_service_endpoint",
    "save_local",
    "search",
    "sign",
    "upload_blob",
    "validate",
    "verify_signature",
];

/// A step accumulates and returns the object; a call ends the chain and is
/// sent. Both lists are the union across the ten libraries, so one
/// constructor serves `records`, `from`, `to` and `get`'s object form alike.
const LAZY: [&str; 6] = ["where", "sort", "limit", "cursor", "did", "collection"];

const IMMEDIATE: [&str; 19] = [
    "run",
    "count",
    "first",
    "records",
    "access",
    "add_member",
    "call",
    "create_invite",
    "create_record",
    "delete",
    "delete_record",
    "is_member",
    "members",
    "put_record",
    "remove_member",
    "set_member",
    "update",
    "upload_blob",
    "write_record",
];

fn surface() -> ApiSurface {
    let mut builders = ["records", "from", "to"]
        .into_iter()
        .map(|name| {
            let mut export = ApiExport::constructor(name);
            for step in LAZY {
                export = export.lazy(step);
            }
            for call in IMMEDIATE {
                export = export.immediate(call);
            }
            export
        })
        .collect::<Vec<_>>();
    builders.extend(FUNCTIONS.into_iter().map(ApiExport::function));
    ApiSurface::new("differential").exports(builders)
}

/// One indexed record, in the envelope shape every library read returns. The
/// second carries a null `cid`, so a script's `if row.cid then` takes both
/// branches across a page.
fn envelope(n: u32) -> Value {
    json!({
        "uri": if n == 1 { "at://did:plc:alice/app.example.post/3kabc1" }
               else { "at://did:plc:alice/app.example.post/3kabc2" },
        "did": "did:plc:alice",
        "collection": "app.example.post",
        "rkey": if n == 1 { "3kabc1" } else { "3kabc2" },
        "cid": if n == 1 { json!("bafyreib2rxk3rh6kzwq") } else { Value::Null },
        "indexed_at": Value::Null,
        "record": {
            "$type": "app.example.post",
            "title": if n == 1 { "Post 1" } else { "Post 2" },
            "tags": [],
            "meta": {},
            "count": n,
            "score": 1.5,
            "reply": Value::Null,
        },
    })
}

fn reference() -> Value {
    json!({ "uri": "at://did:plc:alice/app.example.post/3knew", "cid": "bafynew" })
}

/// The answer for a chain, chosen by the call that ended it.
fn chained(name: &str) -> Value {
    match name {
        "run" | "records" => json!({ "records": [envelope(1), envelope(2)], "cursor": "c2" }),
        "count" => json!(2),
        "first" => envelope(1),
        "members" => json!([{ "did": "did:plc:alice", "access": "admin" }]),
        "is_member" => json!(true),
        "access" => json!("write"),
        "create_invite" => json!({ "token": "tok", "expires_at": Value::Null }),
        "write_record" | "put_record" | "create_record" => reference(),
        "delete" | "delete_record" | "remove_member" | "add_member" | "set_member"
        | "update" => Value::Null,
        _ => json!({ "called": name }),
    }
}

fn answer(function: &str, args: &[Value]) -> Value {
    // An object document — `{args, steps, call}` — is a chain rather than a
    // plain call, and what it returns depends on the call that ended it.
    if let Some(call) = args.first().and_then(|first| first.get("call")) {
        return chained(call["name"].as_str().unwrap_or(""));
    }
    match function {
        "get" | "load" => envelope(1),
        "search" | "list" | "get_labels" | "get_labels_batch" => json!([envelope(1)]),
        "backend" => json!("sqlite"),
        "query" | "raw" => json!({ "rows": [envelope(1)], "cursor": Value::Null }),
        "create" | "put" | "save_local" | "upload_blob" => reference(),
        "delete" | "delete_local" => Value::Null,
        "info" => json!({ "uri": "at://did:plc:alice/space/com.example.forum/main" }),
        "head" | "post" | "patch" | "procedure" => json!({
            "status": 200, "body": "{\"ok\":true}", "headers": { "content-type": "application/json" },
        }),
        "resolve_service_endpoint" => json!("https://pds.example.test"),
        "blob_download" => json!({ "bytes": "aGk=", "mime_type": "text/plain" }),
        "sign" => json!({ "sig": "c2ln" }),
        "verify_signature" | "validate" => json!(true),
        "accept_invite" => json!({ "space": "at://did:plc:alice/space/com.example.forum/main" }),
        "lexicon" => json!({ "lexicon": 1, "id": "app.example.post", "defs": {} }),
        _ => json!({ "called": function }),
    }
}

/// The key every call is appended under, read back by the harness straight
/// from the store's table.
const TRANSCRIPT: &str = "transcript";

/// Append one call. Read-modify-write rather than an append, because the
/// store has no append; both halves are awaited by the host, so the list is
/// whole and ordered by the time the run that produced it ends.
///
/// A failure here **fails the call**. Recording is this fixture's whole
/// purpose, and a swallowed error leaves an empty transcript that every
/// comparison of it then passes — which is a broken harness reporting
/// success, the one outcome worse than a broken harness.
fn record(function: &str, args: &[Value], ctx: &CallContext) -> Result<(), PluginError> {
    let mut calls = match host::kv_get(TRANSCRIPT)? {
        Some(bytes) => serde_json::from_slice::<Vec<Value>>(&bytes)?,
        None => Vec::new(),
    };
    // The caller a library sees is part of what crossed the boundary, so it
    // is compared rather than assumed: the two paths derive it differently
    // and nothing else would notice if they disagreed.
    calls.push(json!({
        "function": function,
        "args": args,
        "caller_did": ctx.caller_did,
        "has_pds_auth": ctx.has_pds_auth,
    }));
    host::kv_set(TRANSCRIPT, &serde_json::to_vec(&calls)?, None).map_err(PluginError::from)
}

fn dispatch(function: &str, args: &[Value], ctx: &CallContext) -> Result<Value, PluginError> {
    // An argument that changes no answer would otherwise leave no trace, so
    // the arguments go down verbatim and in call order.
    record(function, args, ctx)?;

    // Two words a script can ask for on purpose, so the harness compares a
    // refused call as well as an answered one.
    let text = json!(args).to_string();
    if text.contains("nosession") {
        return Err(PluginError::new(
            "NO_SESSION",
            "no PDS session for this caller",
        ));
    }
    if text.contains("boom") {
        return Err(PluginError::new("UPSTREAM", "the library failed on purpose"));
    }

    Ok(answer(function, args))
}
