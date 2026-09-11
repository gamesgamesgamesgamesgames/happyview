//! The raw calling convention between HappyView and a plugin.
//!
//! Exports return a packed `i64` of `(ptr << 32) | len` naming a JSON envelope
//! in the module's linear memory; inputs arrive as a `(ptr, len)` pair the host
//! wrote through the module's own `alloc`. Nothing in here is meant to be
//! called by hand — `library_plugin!` wires it up.

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::envelope::PluginError;
use crate::types::{CallContext, CallInput};
use serde_json::Value;

/// Default guest heap, overridable with `export_abi!(heap = <bytes>)`.
pub const DEFAULT_HEAP_SIZE: usize = 524_288;

/// Bump-allocate `layout` out of a caller-owned heap, returning null when the
/// heap is exhausted. The heap and its cursor live in the plugin crate because
/// `export_abi!` places them there; this is only the arithmetic.
///
/// # Safety
/// `heap` must point at `heap_size` writable bytes that outlive every returned
/// pointer, and `pos` must point at a `usize` used for no other heap.
pub unsafe fn bump_alloc(
    heap: *mut u8,
    heap_size: usize,
    pos: *mut usize,
    layout: core::alloc::Layout,
) -> *mut u8 {
    let align = layout.align();
    let Some(start) = pos.read().checked_add(align - 1).map(|v| v & !(align - 1)) else {
        return core::ptr::null_mut();
    };
    let Some(end) = start.checked_add(layout.size()) else {
        return core::ptr::null_mut();
    };
    if end > heap_size {
        return core::ptr::null_mut();
    }
    pos.write(end);
    heap.add(start)
}

/// Body of the `alloc` export. The host calls this to place its own inputs and
/// responses in guest memory, so it must stay reachable from the module.
pub fn alloc_bytes(size: u32) -> u32 {
    #[cfg(target_arch = "wasm32")]
    {
        if size == 0 {
            return 0;
        }
        let Ok(layout) = core::alloc::Layout::from_size_align(size as usize, 1) else {
            return 0;
        };
        // SAFETY: `layout` has a non-zero size.
        unsafe { alloc::alloc::alloc(layout) as u32 }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = size;
        0
    }
}

/// Body of the `dealloc` export. The bump allocator never reuses memory, so
/// this is a no-op — the export exists because the host calls it.
pub fn dealloc_bytes(_ptr: u32, _size: u32) {}

/// Serialise `value` into guest memory and pack its address for return.
/// Returns 0 when serialisation or allocation fails, which the host reads as
/// "no response".
pub fn return_json<T: Serialize + ?Sized>(value: &T) -> i64 {
    let Ok(bytes) = serde_json::to_vec(value) else {
        return 0;
    };
    write_bytes(&bytes)
}

/// Return `{"ok": value}`.
pub fn return_ok<T: Serialize + ?Sized>(value: &T) -> i64 {
    #[derive(Serialize)]
    struct OkOut<'a, T: ?Sized> {
        ok: &'a T,
    }
    return_json(&OkOut { ok: value })
}

/// Return `{"error": {...}}`.
pub fn return_err(error: &PluginError) -> i64 {
    #[derive(Serialize)]
    struct ErrOut<'a> {
        error: &'a PluginError,
    }
    return_json(&ErrOut { error })
}

fn write_bytes(bytes: &[u8]) -> i64 {
    let len = bytes.len() as u32;
    let ptr = alloc_bytes(len);
    if ptr == 0 {
        return 0;
    }
    // SAFETY: `alloc_bytes` just handed back `len` writable bytes, and the
    // source and destination cannot overlap because the source is a fresh
    // serialisation buffer.
    unsafe {
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as usize as *mut u8, bytes.len());
    }
    ((ptr as i64) << 32) | (len as i64)
}

/// Borrow a `(ptr, len)` input the host wrote into guest memory.
///
/// # Safety
/// `ptr` and `len` must describe a live region of this module's linear memory,
/// and the borrow must not outlive it. Always `0` outside wasm32.
pub unsafe fn read_input<'a>(ptr: u32, len: u32) -> &'a [u8] {
    #[cfg(target_arch = "wasm32")]
    {
        if len == 0 {
            return &[];
        }
        core::slice::from_raw_parts(ptr as usize as *const u8, len as usize)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (ptr, len);
        &[]
    }
}

/// Decode a packed `i64` a host function returned. `None` covers both a packed
/// 0 (the host's "no value") and a body that does not parse as `T`.
pub fn read_packed<T: DeserializeOwned>(packed: i64) -> Option<T> {
    if packed == 0 {
        return None;
    }
    let ptr = (packed >> 32) as u32;
    let len = (packed & 0xFFFF_FFFF) as u32;
    #[cfg(target_arch = "wasm32")]
    {
        // SAFETY: the host allocated this region through our own `alloc` and
        // does not free it; the borrow ends inside this call.
        serde_json::from_slice(unsafe { read_input(ptr, len) }).ok()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (ptr, len);
        None
    }
}

/// The body of the `call` export: decode the input envelope, dispatch, and
/// wrap whatever comes back. Bad input is an envelope, never a panic.
pub fn dispatch_call(
    ptr: u32,
    len: u32,
    handler: fn(&str, &[Value], &CallContext) -> Result<Value, PluginError>,
) -> i64 {
    // SAFETY: the host wrote this region through our `alloc` immediately
    // before calling us and keeps it alive for the duration of the call.
    let bytes = unsafe { read_input(ptr, len) };
    let input: CallInput = match serde_json::from_slice(bytes) {
        Ok(input) => input,
        Err(err) => return return_err(&PluginError::from(err)),
    };
    match handler(&input.function, &input.args, &input.context) {
        Ok(value) => return_ok(&value),
        Err(err) => return_err(&err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;

    #[test]
    fn packed_zero_is_no_value() {
        assert_eq!(read_packed::<Value>(0), None);
        assert_eq!(read_packed::<String>(0), None);
    }

    #[test]
    fn bump_alloc_respects_alignment_and_capacity() {
        let mut heap = [0u8; 64];
        let mut pos = 0usize;
        let base = heap.as_mut_ptr();
        // SAFETY: `base`/`pos` describe the local heap above.
        unsafe {
            let a = bump_alloc(base, 64, &raw mut pos, layout(3, 1));
            assert_eq!(a, base);
            let b = bump_alloc(base, 64, &raw mut pos, layout(4, 4));
            assert_eq!(
                b as usize - base as usize,
                4,
                "second alloc must be aligned"
            );
            assert!(bump_alloc(base, 64, &raw mut pos, layout(1024, 1)).is_null());
        }
    }

    fn layout(size: usize, align: usize) -> core::alloc::Layout {
        core::alloc::Layout::from_size_align(size, align).unwrap()
    }

    #[test]
    fn alloc_is_unavailable_off_wasm_so_returns_are_zero() {
        assert_eq!(alloc_bytes(8), 0);
        assert_eq!(return_ok(&Value::Null), 0);
        dealloc_bytes(0, 0);
    }
}
