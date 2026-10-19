//! Interpreter-plugin fixture. Declares the ABI by hand rather than through
//! `interpreter_plugin!`, so the macro cannot mask a mistake in what the host
//! calls, and reaches every import an interpreter may use.
//!
//! It interprets nothing: `source` is a directive. Anything it does not
//! recognise echoes the whole `execute` input back as the returned value.
//! `host:module_resolve` asks the host for `payload.specifier` imported from
//! `payload.referrer`, the run's own chunk by default.
//!
//! `validate` with `prepare` returns the source's own bytes as an
//! `echo-source` artifact, unless the source starts `no-artifact`, and an
//! import for each line reading `import <specifier>`. `execute` handed an
//! empty `source` and such an artifact runs the decoded text.
//!
//! A source starting `old-sdk` is refused for a missing `handle` whatever
//! its role, as an interpreter that predates roles refuses it.
//!
//! `execute` answers a `test` run whose source starts `predates-tests` with a
//! `BAD_INPUT` envelope, as an interpreter that predates test runs fails to
//! decode the kind.
#![cfg_attr(target_arch = "wasm32", no_std)]
#![allow(static_mut_refs)]

#[cfg(target_arch = "wasm32")]
extern crate alloc;
#[cfg(not(target_arch = "wasm32"))]
extern crate alloc;

#[cfg(target_arch = "wasm32")]
use alloc::{format, string::ToString};
#[cfg(target_arch = "wasm32")]
use core::alloc::{GlobalAlloc, Layout};

use serde_json::{json, Value};

#[cfg(target_arch = "wasm32")]
struct BumpAllocator;
#[cfg(target_arch = "wasm32")]
const HEAP_SIZE: usize = 524_288;
#[cfg(target_arch = "wasm32")]
static mut HEAP: [u8; HEAP_SIZE] = [0; HEAP_SIZE];
#[cfg(target_arch = "wasm32")]
static mut HEAP_POS: usize = 0;

#[cfg(target_arch = "wasm32")]
unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pos = (HEAP_POS + layout.align() - 1) & !(layout.align() - 1);
        if pos + layout.size() > HEAP_SIZE {
            return core::ptr::null_mut();
        }
        HEAP_POS = pos + layout.size();
        HEAP.as_mut_ptr().add(pos)
    }
    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {}
}

#[cfg(target_arch = "wasm32")]
#[global_allocator]
static ALLOCATOR: BumpAllocator = BumpAllocator;

#[cfg(target_arch = "wasm32")]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    core::arch::wasm32::unreachable()
}

#[cfg(target_arch = "wasm32")]
// Explicit import module: from Rust 1.96 the wasm32 linker no longer turns a bare undefined symbol into an import.
#[link(wasm_import_module = "env")]
extern "C" {
    fn host_call_library(
        lib_ptr: i32,
        lib_len: i32,
        fn_ptr: i32,
        fn_len: i32,
        args_ptr: i32,
        args_len: i32,
    ) -> i64;
    fn host_call_library_start(
        lib_ptr: i32,
        lib_len: i32,
        fn_ptr: i32,
        fn_len: i32,
        args_ptr: i32,
        args_len: i32,
    ) -> i32;
    fn host_call_library_wait_any(req_ptr: i32, req_len: i32) -> i64;
    fn host_get_api_surface(lib_ptr: i32, lib_len: i32) -> i64;
    fn host_script_log(req_ptr: i32, req_len: i32) -> i64;
    fn host_job_progress(req_ptr: i32, req_len: i32) -> i64;
    fn host_job_should_stop(req_ptr: i32, req_len: i32) -> i64;
    fn host_job_wait(req_ptr: i32, req_len: i32) -> i64;
    fn host_module_resolve(req_ptr: i32, req_len: i32) -> i64;
}

// The test runner's imports, defined only for a test file's run.
#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "env")]
extern "C" {
    fn host_test_trigger(req_ptr: i32, req_len: i32) -> i64;
    fn host_test_mock(req_ptr: i32, req_len: i32) -> i64;
    fn host_test_calls(req_ptr: i32, req_len: i32) -> i64;
    fn host_test_pds_writes(req_ptr: i32, req_len: i32) -> i64;
    fn host_test_report(req_ptr: i32, req_len: i32) -> i64;
}

#[no_mangle]
pub extern "C" fn alloc(size: u32) -> u32 {
    #[cfg(target_arch = "wasm32")]
    {
        let layout = Layout::from_size_align(size.max(1) as usize, 1).unwrap();
        unsafe { ALLOCATOR.alloc(layout) as u32 }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = size;
        0
    }
}

#[no_mangle]
pub extern "C" fn dealloc(_ptr: u32, _size: u32) {}

fn return_json(value: &Value) -> i64 {
    let text = value.to_string();
    let ptr = alloc(text.len() as u32);
    if ptr == 0 {
        return 0;
    }
    #[cfg(target_arch = "wasm32")]
    unsafe {
        core::ptr::copy_nonoverlapping(text.as_ptr(), ptr as *mut u8, text.len());
    }
    ((ptr as i64) << 32) | (text.len() as i64)
}

fn err(code: &str, message: &str) -> Value {
    json!({"error": {"code": code, "message": message, "retryable": false}})
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn b64_encode(bytes: &[u8]) -> alloc::string::String {
    let mut out = alloc::string::String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(B64[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn b64_decode(text: &str) -> Option<alloc::vec::Vec<u8>> {
    let mut out = alloc::vec::Vec::new();
    let digits: alloc::vec::Vec<u32> = text
        .bytes()
        .filter(|&c| c != b'=')
        .map(|c| B64.iter().position(|&d| d == c).map(|p| p as u32))
        .collect::<Option<_>>()?;
    for group in digits.chunks(4) {
        let n = group
            .iter()
            .enumerate()
            .fold(0u32, |acc, (i, d)| acc | (d << (18 - 6 * i)));
        for i in 0..group.len().saturating_sub(1) {
            out.push((n >> (16 - 8 * i)) as u8);
        }
    }
    Some(out)
}

#[cfg(target_arch = "wasm32")]
fn read_packed(packed: i64) -> Value {
    if packed == 0 {
        return err("HOST_ERROR", "import returned 0");
    }
    let ptr = (packed >> 32) as u32;
    let len = (packed & 0xFFFF_FFFF) as u32;
    let bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) };
    serde_json::from_slice(bytes).unwrap_or_else(|_| err("HOST_ERROR", "import answered non-JSON"))
}

#[cfg(target_arch = "wasm32")]
fn read_input(ptr: u32, len: u32) -> Option<Value> {
    let bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) };
    serde_json::from_slice(bytes).ok()
}

#[cfg(not(target_arch = "wasm32"))]
fn read_input(_ptr: u32, _len: u32) -> Option<Value> {
    None
}

/// Each envelope-returning import, answered whole: a host test asserts on the
/// envelope rather than on this fixture's reading of it.
#[cfg(target_arch = "wasm32")]
mod imports {
    use super::*;

    fn send(import: unsafe extern "C" fn(i32, i32) -> i64, request: &Value) -> Value {
        let body = request.to_string();
        read_packed(unsafe { import(body.as_ptr() as i32, body.len() as i32) })
    }

    pub fn script_log(request: &Value) -> Value {
        send(host_script_log, request)
    }

    pub fn job_progress(request: &Value) -> Value {
        send(host_job_progress, request)
    }

    pub fn job_should_stop() -> Value {
        send(host_job_should_stop, &json!({}))
    }

    pub fn job_wait(request: &Value) -> Value {
        send(host_job_wait, request)
    }

    pub fn module_resolve(request: &Value) -> Value {
        send(host_module_resolve, request)
    }

    pub fn api_surface_of(lib: &str) -> Value {
        read_packed(unsafe { host_get_api_surface(lib.as_ptr() as i32, lib.len() as i32) })
    }

    pub fn call_library(lib: &str, function: &str, args: &Value) -> Value {
        let args = args.to_string();
        read_packed(unsafe {
            host_call_library(
                lib.as_ptr() as i32,
                lib.len() as i32,
                function.as_ptr() as i32,
                function.len() as i32,
                args.as_ptr() as i32,
                args.len() as i32,
            )
        })
    }

    /// The raw handle, 0 included: a host test asserts on what came back.
    pub fn call_library_start(lib: &str, function: &str, args: &str) -> i32 {
        unsafe {
            host_call_library_start(
                lib.as_ptr() as i32,
                lib.len() as i32,
                function.as_ptr() as i32,
                function.len() as i32,
                args.as_ptr() as i32,
                args.len() as i32,
            )
        }
    }

    /// `request` is sent as written, so a test can send one that is malformed.
    pub fn call_library_wait_any(request: &str) -> Value {
        read_packed(unsafe {
            host_call_library_wait_any(request.as_ptr() as i32, request.len() as i32)
        })
    }

    pub fn test_import(name: &str, request: &Value) -> Value {
        let import: unsafe extern "C" fn(i32, i32) -> i64 = match name {
            "trigger" => host_test_trigger,
            "mock" => host_test_mock,
            "calls" => host_test_calls,
            "pds_writes" => host_test_pds_writes,
            "report" => host_test_report,
            _ => return err("BAD_INPUT", "unknown test import"),
        };
        send(import, request)
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod imports {
    use super::*;

    fn unavailable() -> Value {
        err("HOST_ERROR", "not wasm")
    }

    pub fn script_log(_request: &Value) -> Value {
        unavailable()
    }
    pub fn job_progress(_request: &Value) -> Value {
        unavailable()
    }
    pub fn job_should_stop() -> Value {
        unavailable()
    }
    pub fn job_wait(_request: &Value) -> Value {
        unavailable()
    }
    pub fn module_resolve(_request: &Value) -> Value {
        unavailable()
    }
    pub fn api_surface_of(_lib: &str) -> Value {
        unavailable()
    }
    pub fn call_library(_lib: &str, _function: &str, _args: &Value) -> Value {
        unavailable()
    }
    pub fn call_library_start(_lib: &str, _function: &str, _args: &str) -> i32 {
        0
    }
    pub fn call_library_wait_any(_request: &str) -> Value {
        unavailable()
    }

    pub fn test_import(_name: &str, _request: &Value) -> Value {
        unavailable()
    }
}

/// `host:calls`: run `steps` in order against the two concurrent-call imports
/// and return what each answered, so a host test reads the whole `(handle,
/// result)` sequence. A step is one of:
///
/// - `{"start": {"library", "function", "args"}}` — start a call; answers
///   `{"handle": n}`. `"args_raw": "<text>"` sends that text as the args
///   instead, for a request that is not an array.
/// - `{"wait": [handles]}` — one `wait_any` over those handles.
/// - `{"wait_raw": "<text>"}` — one `wait_any` with that text as the request.
/// - `{"wait_all": true}` — `wait_any` over every started, uncollected
///   handle until none is left; answers the list of what each wait returned.
/// - `{"trap": true}` — trap, ending the run with whatever is still in flight.
fn run_calls(steps: &Value) -> Value {
    let mut outstanding: alloc::vec::Vec<i64> = alloc::vec::Vec::new();
    let mut answers: alloc::vec::Vec<Value> = alloc::vec::Vec::new();
    for step in steps.as_array().map(|s| s.as_slice()).unwrap_or(&[]) {
        if let Some(start) = step.get("start") {
            let args = match start.get("args_raw").and_then(Value::as_str) {
                Some(raw) => raw.to_string(),
                None => start
                    .get("args")
                    .cloned()
                    .unwrap_or_else(|| json!([]))
                    .to_string(),
            };
            let handle = imports::call_library_start(
                start["library"].as_str().unwrap_or(""),
                start["function"].as_str().unwrap_or(""),
                &args,
            );
            outstanding.push(handle as i64);
            answers.push(json!({"handle": handle}));
        } else if let Some(handles) = step.get("wait") {
            let answer = imports::call_library_wait_any(&handles.to_string());
            if let Some(done) = answer.get("handle").and_then(Value::as_i64) {
                outstanding.retain(|h| *h != done);
            }
            answers.push(answer);
        } else if let Some(raw) = step.get("wait_raw").and_then(Value::as_str) {
            answers.push(imports::call_library_wait_any(raw));
        } else if step.get("wait_all").is_some() {
            let mut settled = alloc::vec::Vec::new();
            while !outstanding.is_empty() {
                let answer = imports::call_library_wait_any(&json!(outstanding).to_string());
                let Some(done) = answer.get("handle").and_then(Value::as_i64) else {
                    settled.push(answer);
                    break;
                };
                outstanding.retain(|h| *h != done);
                settled.push(answer);
            }
            answers.push(Value::Array(settled));
        } else if step.get("trap").is_some() {
            trap()
        }
    }
    Value::Array(answers)
}

fn trap() -> ! {
    #[cfg(target_arch = "wasm32")]
    {
        core::arch::wasm32::unreachable()
    }
    #[cfg(not(target_arch = "wasm32"))]
    panic!("not wasm")
}

/// A loop no optimiser may drop, for the host's execution-deadline tests.
fn spin_forever() -> ! {
    #[cfg(target_arch = "wasm32")]
    {
        static mut TURNS: u64 = 0;
        loop {
            unsafe {
                let next = core::ptr::read_volatile(&raw const TURNS).wrapping_add(1);
                core::ptr::write_volatile(&raw mut TURNS, next);
            }
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    loop {}
}

/// Spend `iterations` of guest CPU and return, which is what distinguishes a
/// budget that bounds the guest from one that does not. `spin` cannot: a run
/// that never returns looks the same under either.
fn burn(iterations: u64) {
    #[cfg(target_arch = "wasm32")]
    {
        static mut SINK: u64 = 0;
        for i in 0..iterations {
            unsafe {
                let next = core::ptr::read_volatile(&raw const SINK).wrapping_add(i | 1);
                core::ptr::write_volatile(&raw mut SINK, next);
            }
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = iterations;
    }
}

/// Ask for `pages` more pages of linear memory and then trap either way, so
/// the host classifies the store's answer rather than this fixture's.
fn grow_then_trap(pages: usize) -> ! {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = core::arch::wasm32::memory_grow::<0>(pages);
        core::arch::wasm32::unreachable()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = pages;
        panic!("not wasm")
    }
}

fn returned(value: Value, value_kind: &str) -> Value {
    json!({"status": "returned", "value": value, "value_kind": value_kind})
}

fn failure(kind: &str, message: &str, chunk: &str) -> Value {
    let mut out = json!({
        "status": "error",
        "kind": kind,
        "message": message,
        "line": 7,
        // The shapes PUC Lua renders: a named chunk `[string "script"]`, a
        // project file `@<path>` as the bare path.
        "raw": if chunk == "script" {
            format!("[string \"script\"]:7: {message}")
        } else {
            format!("{chunk}:7: {message}")
        },
    });
    if chunk != "script" {
        out["chunk"] = json!(chunk);
    }
    out
}

fn failed(kind: &str, chunk: &str) -> Value {
    failure(kind, &format!("echo: {kind}"), chunk)
}

fn raised(message: &str, chunk: &str) -> Value {
    failure("runtime", message, chunk)
}

/// A test file for the host's own tests: each step calls one test import, or
/// checks a JSON pointer into an earlier answer and reports the result, which
/// is enough for a host test to see triggers, mocks and reports end to end
/// without an assertion library. A `spin` step never returns, for a test of
/// the file's own deadline after the steps before it; a `burn` step spends
/// that many iterations of guest time and answers null.
fn run_test_steps(steps: &Value) -> Value {
    let mut answers: alloc::vec::Vec<Value> = alloc::vec::Vec::new();
    for step in steps.as_array().into_iter().flatten() {
        if step.get("spin").is_some() {
            spin_forever()
        }
        if let Some(iterations) = step.get("burn").and_then(Value::as_u64) {
            burn(iterations);
            answers.push(Value::Null);
            continue;
        }
        let answer = if let Some(check) = step.get("check") {
            let index = check["answer"].as_u64().unwrap_or(0) as usize;
            let earlier = answers.get(index).cloned().unwrap_or(Value::Null);
            let actual = earlier
                .pointer(check["pointer"].as_str().unwrap_or(""))
                .cloned()
                .unwrap_or(Value::Null);
            let passed = actual == check["equals"];
            imports::test_import(
                "report",
                &json!({
                    "path": "",
                    "name": check["name"],
                    "status": if passed { "pass" } else { "fail" },
                    "message": if passed {
                        Value::Null
                    } else {
                        json!(format!("expected {}, got {}", check["equals"], actual))
                    },
                    "line": check["line"],
                    "duration_ms": 0,
                }),
            )
        } else {
            imports::test_import(step["import"].as_str().unwrap_or(""), &step["request"])
        };
        answers.push(answer);
    }
    json!({ "answers": answers })
}

#[no_mangle]
pub extern "C" fn plugin_info() -> i64 {
    return_json(&json!({"ok": {
        "id": "interpreter_echo",
        "name": "Interpreter echo fixture",
        "version": "1.0.0",
        "api_version": "2",
        "required_secrets": [],
        "icon_url": null,
        "config_schema": null,
    }}))
}

#[no_mangle]
pub extern "C" fn execute(ptr: u32, len: u32) -> i64 {
    let Some(input) = read_input(ptr, len) else {
        return return_json(&err("BAD_INPUT", "invalid JSON"));
    };
    let chunk = input["chunk_name"].as_str().unwrap_or("script").to_string();
    let source = match (input["source"].as_str(), input.get("artifact")) {
        (Some(""), Some(artifact)) if artifact["format"] == "echo-source" => {
            match artifact["bytes"].as_str().and_then(b64_decode) {
                Some(bytes) => alloc::string::String::from_utf8(bytes).unwrap_or_default(),
                None => return return_json(&err("BAD_INPUT", "echo: undecodable artifact")),
            }
        }
        (Some(""), Some(artifact)) => {
            return return_json(&json!({"ok": raised(
                &format!("echo: cannot load a {} artifact", artifact["format"]),
                &chunk,
            )}));
        }
        (source, _) => source.unwrap_or("").to_string(),
    };
    let payload = input["input"].clone();

    if source == "spin" {
        spin_forever()
    }
    if let Some(pages) = source.strip_prefix("grow:") {
        grow_then_trap(pages.parse().unwrap_or(1))
    }
    // Falls through to the echo below, so the run returns a value.
    if let Some(iterations) = source.strip_prefix("burn:") {
        burn(iterations.parse().unwrap_or(0));
    }

    if input["kind"] == "test" && source.starts_with("predates-tests") {
        return return_json(&err("BAD_INPUT", "unknown variant `test`"));
    }
    if let Some(steps) = source.strip_prefix("test:") {
        let steps: Value = serde_json::from_str(steps).unwrap_or(Value::Null);
        return return_json(&json!({"ok": returned(run_test_steps(&steps), "object")}));
    }

    let out = if let Some(kind) = source.strip_prefix("error:") {
        failed(kind, &chunk)
    } else if let Some(literal) = source.strip_prefix("returns:") {
        // A value chosen by the caller, for a host branch that reads the
        // returned value's own fields rather than only its kind.
        let value: Value = serde_json::from_str(literal).unwrap_or(Value::Null);
        let kind = if value.is_object() { "object" } else { "other" };
        returned(value, kind)
    } else if let Some(kind) = source.strip_prefix("value:") {
        let value = match kind {
            "none" => Value::Null,
            "object" => input["context"].clone(),
            _ => payload,
        };
        returned(value, kind)
    } else {
        match source.as_str() {
            "host:script_log" => returned(
                imports::script_log(&json!({
                    "level": payload["level"].as_str().unwrap_or("info"),
                    "message": payload["message"].as_str().unwrap_or("from the fixture"),
                    "fields": payload["fields"].clone(),
                })),
                "other",
            ),
            // The whole `execute` input, through the one channel a run has
            // that does not pass through its outcome. A runner that reads
            // only parts of what a script returned cannot be handed the
            // input back, so it reads this line's `fields` instead.
            "host:log_input" => returned(
                imports::script_log(&json!({
                    "level": "info",
                    "message": "input",
                    "fields": input.clone(),
                })),
                "other",
            ),
            "host:job_progress" => {
                returned(imports::job_progress(&json!({"data": payload})), "other")
            }
            "host:job_should_stop" => returned(imports::job_should_stop(), "other"),
            "host:job_wait" => returned(
                imports::job_wait(&json!({"seconds": payload["seconds"].as_f64().unwrap_or(0.0)})),
                "other",
            ),
            "host:library" => {
                let lib = payload["library"].as_str().unwrap_or("");
                let function = payload["function"].as_str().unwrap_or("");
                let args = payload.get("args").cloned().unwrap_or_else(|| json!([]));
                returned(
                    json!({
                        "surface": imports::api_surface_of(lib),
                        "call": imports::call_library(lib, function, &args),
                    }),
                    "object",
                )
            }
            "host:calls" => returned(run_calls(&payload["steps"]), "other"),
            // The `require` contract a language gives a library call: the
            // value on success and a raise on an error envelope. A host test
            // driving a library through a real request reads the library's own
            // answer as the response, and its refusals as failed runs.
            "host:require" => {
                let lib = payload["library"].as_str().unwrap_or("");
                let function = payload["function"].as_str().unwrap_or("");
                let args = payload.get("args").cloned().unwrap_or_else(|| json!([]));
                let envelope = imports::call_library(lib, function, &args);
                match envelope.get("ok") {
                    Some(value) => {
                        let kind = if value.is_object() { "object" } else { "other" };
                        returned(value.clone(), kind)
                    }
                    None => raised(
                        &format!(
                            "{} - {}",
                            envelope["error"]["code"].as_str().unwrap_or("HOST_ERROR"),
                            envelope["error"]["message"].as_str().unwrap_or(""),
                        ),
                        &chunk,
                    ),
                }
            }
            // The referrer defaults to the run's own chunk, which is what an
            // interpreter sends for an import made by the handler itself.
            "host:module_resolve" => returned(
                imports::module_resolve(&json!({
                    "specifier": payload["specifier"].as_str().unwrap_or(""),
                    "referrer": payload["referrer"].as_str().unwrap_or(&chunk),
                })),
                "object",
            ),
            _ => returned(input, "object"),
        }
    };
    return_json(&json!({"ok": out}))
}

#[no_mangle]
pub extern "C" fn validate(ptr: u32, len: u32) -> i64 {
    let Some(input) = read_input(ptr, len) else {
        return return_json(&err("BAD_INPUT", "invalid JSON"));
    };
    let source = input["source"].as_str().unwrap_or("");
    let prepare = input["prepare"].as_bool().unwrap_or(false);
    let role = input["role"].as_str().unwrap_or("handler");
    let out = if source.starts_with("probe-globals") {
        // Reports the guard list validation was handed, so the host can see
        // that it arrived rather than trusting that it was sent.
        let names: alloc::vec::Vec<&str> = input["removed_globals"]
            .as_array()
            .map(|entries| entries.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        json!({"valid": false, "errors": [
            {"kind": "runtime", "message": names.join(",")}
        ]})
    } else if source.starts_with("probe-log") {
        // Reports what `host_script_log` answered, so the host can assert
        // that validation reaches no run and writes no log line.
        let envelope = imports::script_log(&json!({
            "level": "info",
            "message": "from validate",
            "fields": Value::Null,
        }));
        let code = envelope["error"]["code"].as_str().unwrap_or("ok");
        json!({"valid": false, "errors": [{"kind": "runtime", "message": code}]})
    } else if source.starts_with("probe-module") {
        // Reports what `host_module_resolve` answered, so the host can assert
        // that validation has no run whose snapshot an import could read.
        let envelope = imports::module_resolve(&json!({
            "specifier": "./probe",
            "referrer": "src/probe.echo",
        }));
        let code = envelope["error"]["code"].as_str().unwrap_or("ok");
        json!({"valid": false, "errors": [{"kind": "runtime", "message": code}]})
    } else if source.starts_with("invalid") {
        json!({"valid": false, "errors": [
            {"kind": "syntax", "line": 1, "message": "echo: the source says it is invalid"}
        ]})
    } else if source.starts_with("old-sdk") {
        // An interpreter built before `role` existed: it demands `handle`
        // of every chunk, module or not.
        json!({"valid": false, "errors": [
            {"kind": "missing_handle", "message": "script must define a handle() function"}
        ]})
    } else if source.starts_with("no-handle") && role == "handler" {
        json!({"valid": false, "errors": [
            {"kind": "missing_handle", "message": "script must define a handle() function"}
        ]})
    } else {
        let mut out = json!({"valid": true});
        if prepare && !source.starts_with("no-artifact") {
            // Deliberately wrong: the host must overwrite both.
            out["artifact"] = json!({
                "interpreter": "forged",
                "version": "0",
                "engine": "echo-1",
                "format": "echo-source",
                "bytes": b64_encode(source.as_bytes()),
            });
        }
        if prepare {
            let imports: alloc::vec::Vec<Value> = source
                .lines()
                .enumerate()
                .filter_map(|(i, line)| {
                    line.strip_prefix("import ")
                        .map(|spec| json!({"specifier": spec.trim(), "line": i + 1}))
                })
                .collect();
            if !imports.is_empty() {
                out["imports"] = Value::Array(imports);
            }
        }
        out
    };
    return_json(&json!({"ok": out}))
}
