//! Library-plugin fixture. Exercises every library export and both
//! library host imports. Registered under whatever id the test chooses;
//! `recurse` takes its own id as an argument for that reason.
#![cfg_attr(target_arch = "wasm32", no_std)]
#![allow(static_mut_refs)]

#[cfg(target_arch = "wasm32")]
extern crate alloc;

#[cfg(target_arch = "wasm32")]
use alloc::{format, string::ToString};
#[cfg(target_arch = "wasm32")]
use core::alloc::{GlobalAlloc, Layout};

use serde_json::{json, Value};

#[cfg(target_arch = "wasm32")]
struct BumpAllocator;
#[cfg(target_arch = "wasm32")]
const HEAP_SIZE: usize = 262_144;
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
    loop {}
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
    fn host_get_api_surface(lib_ptr: i32, lib_len: i32) -> i64;
    fn host_db_query(sql_ptr: i32, sql_len: i32, params_ptr: i32, params_len: i32) -> i64;
    fn host_db_execute(sql_ptr: i32, sql_len: i32, params_ptr: i32, params_len: i32) -> i64;
}

#[no_mangle]
pub extern "C" fn alloc(size: u32) -> u32 {
    #[cfg(target_arch = "wasm32")]
    {
        let layout = Layout::from_size_align(size as usize, 1).unwrap();
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

fn return_json(s: &str) -> i64 {
    let ptr = alloc(s.len() as u32);
    if ptr == 0 {
        return 0;
    }
    #[cfg(target_arch = "wasm32")]
    unsafe {
        core::ptr::copy_nonoverlapping(s.as_ptr(), ptr as *mut u8, s.len());
    }
    ((ptr as i64) << 32) | (s.len() as i64)
}

fn read_packed(packed: i64) -> Option<Value> {
    if packed == 0 {
        return None;
    }
    let ptr = (packed >> 32) as u32;
    let len = (packed & 0xFFFF_FFFF) as u32;
    #[cfg(target_arch = "wasm32")]
    {
        let bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) };
        serde_json::from_slice(bytes).ok()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (ptr, len);
        None
    }
}

fn err(code: &str, message: &str) -> Value {
    json!({"error": {"code": code, "message": message, "retryable": false}})
}

#[cfg(target_arch = "wasm32")]
fn call_library(lib: &str, function: &str, args: &Value) -> Value {
    let args = serde_json::to_string(args).unwrap_or_default();
    let packed = unsafe {
        host_call_library(
            lib.as_ptr() as i32,
            lib.len() as i32,
            function.as_ptr() as i32,
            function.len() as i32,
            args.as_ptr() as i32,
            args.len() as i32,
        )
    };
    read_packed(packed).unwrap_or_else(|| err("HOST_ERROR", "host_call_library returned 0"))
}

#[cfg(target_arch = "wasm32")]
fn get_api_surface_of(lib: &str) -> Value {
    let packed = unsafe { host_get_api_surface(lib.as_ptr() as i32, lib.len() as i32) };
    read_packed(packed).unwrap_or_else(|| err("HOST_ERROR", "host_get_api_surface returned 0"))
}

#[cfg(not(target_arch = "wasm32"))]
fn call_library(_lib: &str, _function: &str, _args: &Value) -> Value {
    err("HOST_ERROR", "not wasm")
}
#[cfg(not(target_arch = "wasm32"))]
fn get_api_surface_of(_lib: &str) -> Value {
    err("HOST_ERROR", "not wasm")
}

#[cfg(target_arch = "wasm32")]
fn db_call(execute: bool, sql: &str, params: &Value) -> Value {
    let params = serde_json::to_string(params).unwrap_or_default();
    let packed = unsafe {
        if execute {
            host_db_execute(
                sql.as_ptr() as i32,
                sql.len() as i32,
                params.as_ptr() as i32,
                params.len() as i32,
            )
        } else {
            host_db_query(
                sql.as_ptr() as i32,
                sql.len() as i32,
                params.as_ptr() as i32,
                params.len() as i32,
            )
        }
    };
    read_packed(packed).unwrap_or_else(|| err("HOST_ERROR", "host_db_* returned 0"))
}

#[cfg(not(target_arch = "wasm32"))]
fn db_call(_execute: bool, _sql: &str, _params: &Value) -> Value {
    err("HOST_ERROR", "not wasm")
}

#[no_mangle]
pub extern "C" fn plugin_info() -> i64 {
    return_json(
        r#"{"ok":{"id":"test_library","name":"Test Library","version":"1.0.0","api_version":"2","required_secrets":[],"icon_url":null,"config_schema":null}}"#,
    )
}

#[no_mangle]
pub extern "C" fn get_api_surface() -> i64 {
    let surface = json!({"ok": {
        "namespace": "testlib",
        "description": "Fixture library",
        "exports": [
            {"name": "echo", "kind": "function", "description": "Return the argument",
             "params": [{"name": "value", "type": "any"}], "returns": {"type": "any"}},
            {"name": "add", "kind": "function",
             "params": [{"name": "a", "type": "number"}, {"name": "b", "type": "number"}],
             "returns": {"type": "number"}},
            {"name": "whoami", "kind": "function", "params": [], "returns": {"type": "string?"}},
            {"name": "call_other", "kind": "function",
             "params": [{"name": "lib", "type": "string"}, {"name": "fn", "type": "string"}, {"name": "args", "type": "any[]"}]},
            {"name": "surface_of", "kind": "function", "params": [{"name": "lib", "type": "string"}]},
            {"name": "recurse", "kind": "function", "params": [{"name": "self_id", "type": "string"}]},
            {"name": "sql_query", "kind": "function",
             "params": [{"name": "sql", "type": "string"}, {"name": "params", "type": "any[]"}],
             "returns": {"type": "object[]"}},
            {"name": "sql_execute", "kind": "function",
             "params": [{"name": "sql", "type": "string"}, {"name": "params", "type": "any[]"}],
             "returns": {"type": "object"}},
            {"name": "VERSION", "kind": "constant", "type": "string"}
        ],
        "types": []
    }});
    return_json(&surface.to_string())
}

#[no_mangle]
pub extern "C" fn call(ptr: u32, len: u32) -> i64 {
    #[cfg(target_arch = "wasm32")]
    let input: Value = {
        let bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) };
        match serde_json::from_slice(bytes) {
            Ok(v) => v,
            Err(_) => return return_json(&err("BAD_INPUT", "invalid JSON").to_string()),
        }
    };
    #[cfg(not(target_arch = "wasm32"))]
    let input: Value = {
        let _ = (ptr, len);
        Value::Null
    };

    let function = input["function"].as_str().unwrap_or("");
    let args = input["args"].as_array().cloned().unwrap_or_default();
    let context = &input["context"];

    let out = match function {
        "echo" => json!({"ok": args.first().cloned().unwrap_or(Value::Null)}),
        "add" => {
            let a = args.first().and_then(Value::as_f64).unwrap_or(0.0);
            let b = args.get(1).and_then(Value::as_f64).unwrap_or(0.0);
            json!({"ok": a + b})
        }
        "whoami" => json!({"ok": context["caller_did"].clone()}),
        "call_other" => {
            let lib = args.first().and_then(Value::as_str).unwrap_or("");
            let f = args.get(1).and_then(Value::as_str).unwrap_or("");
            let inner = args.get(2).cloned().unwrap_or_else(|| json!([]));
            call_library(lib, f, &inner)
        }
        "surface_of" => {
            let lib = args.first().and_then(Value::as_str).unwrap_or("");
            get_api_surface_of(lib)
        }
        "recurse" => {
            let me = args.first().and_then(Value::as_str).unwrap_or("");
            call_library(me, "recurse", &json!([me]))
        }
        "sql_query" => {
            let sql = args.first().and_then(Value::as_str).unwrap_or("");
            let params = args.get(1).cloned().unwrap_or_else(|| json!([]));
            db_call(false, sql, &params)
        }
        "sql_execute" => {
            let sql = args.first().and_then(Value::as_str).unwrap_or("");
            let params = args.get(1).cloned().unwrap_or_else(|| json!([]));
            db_call(true, sql, &params)
        }
        other => err("UNKNOWN_FUNCTION", &format!("no such function: {other}")),
    };
    return_json(&out.to_string())
}
