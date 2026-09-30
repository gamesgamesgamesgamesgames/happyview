//! The raw calling convention between HappyView and a plugin.
//!
//! Exports return a packed `i64` of `(ptr << 32) | len` naming a JSON envelope
//! in the module's linear memory; inputs arrive as a `(ptr, len)` pair the host
//! wrote through the module's own `alloc`. Nothing in here is meant to be
//! called by hand — `library_plugin!` wires it up.

use alloc::vec::Vec;
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::envelope::PluginError;
use crate::types::{
    CallContext, CallInput, ExecuteInput, ExecuteOutput, ValidateInput, ValidateOutput,
};
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

/// `size` bytes from the crate's global allocator, or null. The layout is
/// always byte-aligned, so `free_raw` can reconstruct it from the length the
/// host hands back.
#[cfg(any(target_arch = "wasm32", test))]
fn alloc_raw(size: usize) -> *mut u8 {
    if size == 0 {
        return core::ptr::null_mut();
    }
    let Ok(layout) = core::alloc::Layout::from_size_align(size, 1) else {
        return core::ptr::null_mut();
    };
    // SAFETY: `layout` has a non-zero size.
    unsafe { alloc::alloc::alloc(layout) }
}

/// # Safety
/// `ptr` and `size` must be a pair [`alloc_raw`] returned and that has not
/// already been freed.
#[cfg(any(target_arch = "wasm32", test))]
unsafe fn free_raw(ptr: *mut u8, size: usize) {
    if ptr.is_null() || size == 0 {
        return;
    }
    let Ok(layout) = core::alloc::Layout::from_size_align(size, 1) else {
        return;
    };
    alloc::alloc::dealloc(ptr, layout)
}

/// Body of the `alloc` export. The host calls this to place its own inputs and
/// responses in guest memory, so it must stay reachable from the module.
pub fn alloc_bytes(size: u32) -> u32 {
    #[cfg(target_arch = "wasm32")]
    {
        alloc_raw(size as usize) as u32
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

/// Body of the `dealloc` export under `export_abi!(alloc = system)`, where the
/// memory really is returned: an interpreter allocates and frees for the whole
/// run, so a heap that only ever grows would end the run rather than the call.
pub fn free_bytes(ptr: u32, size: u32) {
    #[cfg(target_arch = "wasm32")]
    {
        // SAFETY: the host passes back a pair `alloc` returned, once.
        unsafe { free_raw(ptr as usize as *mut u8, size as usize) }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (ptr, size);
    }
}

/// Serialise `value` into guest memory and pack its address for return.
/// Returns 0 when serialisation or allocation fails, which the host reads as
/// "no response".
pub fn return_json<T: Serialize + ?Sized>(value: &T) -> i64 {
    let Ok(bytes) = serde_json::to_vec(value) else {
        return 0;
    };
    write_bytes(&bytes)
}

#[derive(Serialize)]
struct OkOut<'a, T: ?Sized> {
    ok: &'a T,
}

#[derive(Serialize)]
struct ErrOut<'a> {
    error: &'a PluginError,
}

/// Return `{"ok": value}`.
pub fn return_ok<T: Serialize + ?Sized>(value: &T) -> i64 {
    return_json(&OkOut { ok: value })
}

/// Return `{"error": {...}}`.
pub fn return_err(error: &PluginError) -> i64 {
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

/// Decode one JSON object, dispatch, and serialise the envelope. Separate from
/// the exports that call it because this half needs no linear memory, which is
/// what makes the contract testable off wasm32.
///
/// Input it cannot decode becomes a `BAD_INPUT` envelope, never a panic. An
/// envelope that will not serialise becomes empty, which the host reads as no
/// response.
pub fn dispatch_bytes<I, O>(bytes: &[u8], handler: fn(&I) -> Result<O, PluginError>) -> Vec<u8>
where
    I: DeserializeOwned,
    O: Serialize,
{
    let input: I = match serde_json::from_slice(bytes) {
        Ok(input) => input,
        Err(err) => {
            let error = PluginError::from(err);
            return serde_json::to_vec(&ErrOut { error: &error }).unwrap_or_default();
        }
    };
    match handler(&input) {
        Ok(value) => serde_json::to_vec(&OkOut { ok: &value }).unwrap_or_default(),
        Err(error) => serde_json::to_vec(&ErrOut { error: &error }).unwrap_or_default(),
    }
}

/// The body of an export that takes one JSON object and returns one value:
/// decode, dispatch, wrap. `auth_plugin!` uses it for its four input-taking exports.
pub fn dispatch_input<I, O>(ptr: u32, len: u32, handler: fn(&I) -> Result<O, PluginError>) -> i64
where
    I: DeserializeOwned,
    O: Serialize,
{
    // SAFETY: the host wrote this region through our `alloc` immediately
    // before calling us and keeps it alive for the duration of the call.
    let bytes = unsafe { read_input(ptr, len) };
    write_bytes(&dispatch_bytes(bytes, handler))
}

/// The body of the `execute` export. Typed rather than generic so a handler
/// with the wrong signature is refused in the plugin crate that wrote it.
pub fn dispatch_execute(
    ptr: u32,
    len: u32,
    handler: fn(&ExecuteInput) -> Result<ExecuteOutput, PluginError>,
) -> i64 {
    dispatch_input(ptr, len, handler)
}

/// The body of the `validate` export.
pub fn dispatch_validate(
    ptr: u32,
    len: u32,
    handler: fn(&ValidateInput) -> Result<ValidateOutput, PluginError>,
) -> i64 {
    dispatch_input(ptr, len, handler)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ScriptKind, ScriptValueKind};
    use crate::wire::{ExecuteLimits, Response};
    use alloc::string::{String, ToString};
    use core::alloc::{GlobalAlloc, Layout};
    use core::sync::atomic::{AtomicUsize, Ordering};

    /// Counts frees of exactly the probe layout, which is the one
    /// `alloc_raw` uses at that size and which the test below is alone in
    /// asking for.
    struct CountingAllocator;

    const PROBE_SIZE: usize = 8 * 1024;
    static PROBE_FREES: AtomicUsize = AtomicUsize::new(0);

    // SAFETY: every call is forwarded to the system allocator unchanged.
    unsafe impl GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            std::alloc::System.alloc(layout)
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            if layout.size() == PROBE_SIZE && layout.align() == 1 {
                PROBE_FREES.fetch_add(1, Ordering::Relaxed);
            }
            std::alloc::System.dealloc(ptr, layout)
        }
    }

    #[global_allocator]
    static ALLOCATOR: CountingAllocator = CountingAllocator;

    /// The `alloc = system` pair returns what it takes, so a run may churn
    /// far more than the bump heap holds. Sixteen times over: the bump
    /// allocator would hand back null a sixteenth of the way in.
    #[test]
    fn the_system_allocator_pair_frees_what_it_allocates() {
        let rounds = 16 * DEFAULT_HEAP_SIZE / PROBE_SIZE;
        let before = PROBE_FREES.load(Ordering::Relaxed);
        for _ in 0..rounds {
            let ptr = alloc_raw(PROBE_SIZE);
            assert!(!ptr.is_null());
            // SAFETY: the pointer is this iteration's own allocation.
            unsafe { free_raw(ptr, PROBE_SIZE) };
        }
        assert!(PROBE_FREES.load(Ordering::Relaxed) - before >= rounds);
    }

    #[test]
    fn a_zero_sized_allocation_is_null_and_freeing_it_is_a_no_op() {
        assert!(alloc_raw(0).is_null());
        // SAFETY: a null pointer with a zero length is the documented no-op.
        unsafe { free_raw(core::ptr::null_mut(), 0) };
        free_bytes(0, 0);
    }

    fn execute_input(source: &str) -> ExecuteInput {
        ExecuteInput {
            source: source.to_string(),
            kind: ScriptKind::XrpcQuery,
            input: Value::Null,
            context: Default::default(),
            libraries: alloc::vec::Vec::new(),
            limits: ExecuteLimits {
                instructions: Some(1000),
                memory_bytes: 1024,
            },
            removed_globals: alloc::vec::Vec::new(),
        }
    }

    fn echo(input: &ExecuteInput) -> Result<ExecuteOutput, PluginError> {
        Ok(ExecuteOutput::Returned {
            value: Value::String(input.source.clone()),
            value_kind: ScriptValueKind::Other,
        })
    }

    fn accept(_input: &ValidateInput) -> Result<ValidateOutput, PluginError> {
        Ok(ValidateOutput::valid())
    }

    #[test]
    fn the_execute_envelope_round_trips_an_input_the_handler_saw() {
        let bytes = serde_json::to_vec(&execute_input("return 1")).unwrap();
        let envelope = dispatch_bytes(&bytes, echo);
        let response: Response<ExecuteOutput> = serde_json::from_slice(&envelope).unwrap();
        assert_eq!(
            response.into_result().unwrap(),
            ExecuteOutput::Returned {
                value: Value::String("return 1".to_string()),
                value_kind: ScriptValueKind::Other,
            }
        );
    }

    #[test]
    fn the_validate_envelope_round_trips() {
        let bytes = serde_json::to_vec(&ValidateInput {
            source: "return 1".to_string(),
            ..Default::default()
        })
        .unwrap();
        let envelope = dispatch_bytes(&bytes, accept);
        let response: Response<ValidateOutput> = serde_json::from_slice(&envelope).unwrap();
        assert_eq!(response.into_result().unwrap(), ValidateOutput::valid());
    }

    /// Input the guest cannot parse is the host's mistake to hear about, not
    /// a trap it has to classify.
    #[test]
    fn unparseable_input_becomes_a_bad_input_envelope() {
        for bad in [&b"not json"[..], b"", b"{\"source\":42}"] {
            let envelope = dispatch_bytes(bad, echo);
            let response: Response<ExecuteOutput> = serde_json::from_slice(&envelope).unwrap();
            let err = response.into_result().unwrap_err();
            assert_eq!(err.code, "BAD_INPUT", "{bad:?}");
        }
        let envelope = dispatch_bytes(&b"not json"[..], accept);
        let response: Response<ValidateOutput> = serde_json::from_slice(&envelope).unwrap();
        assert_eq!(response.into_result().unwrap_err().code, "BAD_INPUT");
    }

    /// A handler's own error travels as its own code rather than the
    /// decoder's.
    #[test]
    fn a_handler_error_keeps_its_code() {
        fn refuse(_input: &ExecuteInput) -> Result<ExecuteOutput, PluginError> {
            Err(PluginError::host("no interpreter"))
        }
        let bytes = serde_json::to_vec(&execute_input("x")).unwrap();
        let response: Response<ExecuteOutput> =
            serde_json::from_slice(&dispatch_bytes(&bytes, refuse)).unwrap();
        assert_eq!(response.into_result().unwrap_err().code, "HOST_ERROR");
    }

    /// The two exports themselves cannot reach linear memory off wasm32, so
    /// what is pinned here is that they answer rather than panic.
    #[test]
    fn the_interpreter_exports_answer_off_wasm() {
        assert_eq!(dispatch_execute(0, 0, echo), 0);
        assert_eq!(dispatch_validate(0, 0, accept), 0);
    }

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
