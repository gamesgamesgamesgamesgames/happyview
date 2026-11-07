use crate::plugin::host::PluginState;
use thiserror::Error;
use wasmtime::Store;

/// The envelope every plugin export returns: `{"ok": result}` or
/// `{"error": {code, message, retryable}}`.
///
/// Defined in the SDK, which the guests build against, so the two cannot drift.
/// The names are the host's own: `PluginResponse` for the envelope and
/// `PluginEnvelopeError` for the error inside it.
pub use happyview_plugin_sdk::wire::{
    PluginError as PluginEnvelopeError, Response as PluginResponse,
};

#[derive(Debug, Error)]
pub enum MemoryError {
    #[error("Memory allocation failed: alloc returned 0")]
    AllocationFailed,
    #[error(
        "Memory access out of bounds: offset {offset} + length {length} exceeds memory size {size}"
    )]
    OutOfBounds {
        offset: usize,
        length: usize,
        size: usize,
    },
    #[error("WASM trap during memory operation: {0}")]
    Trap(#[from] wasmtime::Error),
}

/// Write data to WASM guest memory by calling alloc and copying bytes.
/// Returns (ptr, len) tuple on success.
pub async fn write_to_guest(
    store: &mut Store<PluginState>,
    data: &[u8],
) -> Result<(u32, u32), MemoryError> {
    let len = data.len() as u32;
    if len == 0 {
        return Ok((0, 0));
    }

    let alloc = store
        .data()
        .alloc
        .as_ref()
        .ok_or(MemoryError::AllocationFailed)?
        .clone();
    let memory = store.data().memory.ok_or(MemoryError::AllocationFailed)?;

    let ptr = alloc.call_async(&mut *store, len).await?;
    if ptr == 0 {
        return Err(MemoryError::AllocationFailed);
    }

    let mem_size = memory.data_size(&*store);
    let start = ptr as usize;
    let end = start
        .checked_add(len as usize)
        .ok_or(MemoryError::OutOfBounds {
            offset: start,
            length: len as usize,
            size: mem_size,
        })?;

    if end > mem_size {
        return Err(MemoryError::OutOfBounds {
            offset: start,
            length: len as usize,
            size: mem_size,
        });
    }

    memory.data_mut(&mut *store)[start..end].copy_from_slice(data);
    Ok((ptr, len))
}

/// Read data from WASM guest memory at the given pointer and length.
pub fn read_from_guest(
    store: &Store<PluginState>,
    ptr: u32,
    len: u32,
) -> Result<Vec<u8>, MemoryError> {
    if len == 0 {
        return Ok(Vec::new());
    }

    let memory = store.data().memory.ok_or(MemoryError::AllocationFailed)?;
    let mem_size = memory.data_size(store);
    let start = ptr as usize;
    let end = start
        .checked_add(len as usize)
        .ok_or(MemoryError::OutOfBounds {
            offset: start,
            length: len as usize,
            size: mem_size,
        })?;

    if end > mem_size {
        return Err(MemoryError::OutOfBounds {
            offset: start,
            length: len as usize,
            size: mem_size,
        });
    }

    Ok(memory.data(store)[start..end].to_vec())
}

/// Deallocate guest memory by calling the dealloc function.
pub async fn dealloc_guest(
    store: &mut Store<PluginState>,
    ptr: u32,
    len: u32,
) -> Result<(), MemoryError> {
    if len == 0 {
        return Ok(());
    }

    let dealloc = store
        .data()
        .dealloc
        .as_ref()
        .ok_or(MemoryError::AllocationFailed)?
        .clone();
    dealloc.call_async(&mut *store, (ptr, len)).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memory_error_display() {
        let err = MemoryError::AllocationFailed;
        assert!(err.to_string().contains("alloc"));

        let err = MemoryError::OutOfBounds {
            offset: 100,
            length: 50,
            size: 120,
        };
        assert!(err.to_string().contains("100"));
    }

    /// The envelope is the SDK's, but the host reads it through these names,
    /// so a rename there would fail here rather than at a call site.
    #[test]
    fn test_plugin_response_ok_parses() {
        let json = r#"{"ok": "hello"}"#;
        let resp: PluginResponse<String> = serde_json::from_str(json).unwrap();
        let result = resp.into_result();
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "hello");
    }

    #[test]
    fn test_plugin_response_error_parses() {
        let json =
            r#"{"error": {"code": "AUTH_FAILED", "message": "Invalid token", "retryable": true}}"#;
        let resp: PluginResponse<String> = serde_json::from_str(json).unwrap();
        let result = resp.into_result();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.code, "AUTH_FAILED");
        assert!(err.retryable);
    }
}
