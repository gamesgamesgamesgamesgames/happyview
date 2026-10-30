//! The preview-1 context a plugin runs against: no arguments, environment,
//! preopens or network, and standard output and error routed into the plugin
//! log. The capability check on the import section decides what a module may
//! link; this context decides which descriptors answer, which is what makes
//! `wasi:stdio` a log grant rather than a file grant.

use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use bytes::Bytes;
use tokio::io::AsyncWrite;
use wasmtime_wasi::cli::{IsTerminal, StdoutStream};
use wasmtime_wasi::p1::WasiP1Ctx;
use wasmtime_wasi::p2::{OutputStream, Pollable, StreamResult};

use super::LogLevel;
use crate::db::DatabaseBackend;

pub fn build_context(
    plugin_id: &str,
    db: Option<sqlx::AnyPool>,
    backend: DatabaseBackend,
) -> WasiP1Ctx {
    wasmtime_wasi::WasiCtxBuilder::new()
        .stdout(PluginLogStream::new(
            plugin_id,
            LogLevel::Info,
            db.clone(),
            backend,
        ))
        .stderr(PluginLogStream::new(plugin_id, LogLevel::Warn, db, backend))
        .build_p1()
}

/// Forwards each complete line written to it as one plugin log entry.
/// Writes complete synchronously, so a line is logged before `fd_write`
/// returns to the guest rather than on a task the guest cannot observe.
#[derive(Clone)]
pub struct PluginLogStream {
    plugin_id: String,
    level: LogLevel,
    db: Option<sqlx::AnyPool>,
    backend: DatabaseBackend,
    pending: Arc<Mutex<Vec<u8>>>,
}

const LINE_PERMIT: usize = 64 * 1024;

impl PluginLogStream {
    pub fn new(
        plugin_id: &str,
        level: LogLevel,
        db: Option<sqlx::AnyPool>,
        backend: DatabaseBackend,
    ) -> Self {
        Self {
            plugin_id: plugin_id.to_string(),
            level,
            db,
            backend,
            pending: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn push(&self, bytes: &[u8]) {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        pending.extend_from_slice(bytes);
        while let Some(newline) = pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = pending.drain(..=newline).collect();
            self.emit(&line[..line.len() - 1]);
        }
        // A guest that writes without ever sending a newline would otherwise
        // grow this buffer without bound.
        if pending.len() >= LINE_PERMIT {
            let line = std::mem::take(&mut *pending);
            self.emit(&line);
        }
    }

    fn emit(&self, line: &[u8]) {
        let text = String::from_utf8_lossy(line);
        let text = text.trim_end_matches('\r');
        if text.is_empty() {
            return;
        }
        super::log(
            &self.plugin_id,
            self.level,
            text,
            self.db.clone(),
            self.backend,
        );
    }
}

impl Drop for PluginLogStream {
    fn drop(&mut self) {
        // The last handle flushes whatever was written without a trailing
        // newline, so a guest's final unterminated line is not lost.
        if Arc::strong_count(&self.pending) == 1 {
            let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            let rest = std::mem::take(&mut *pending);
            drop(pending);
            if !rest.is_empty() {
                self.emit(&rest);
            }
        }
    }
}

impl IsTerminal for PluginLogStream {
    fn is_terminal(&self) -> bool {
        false
    }
}

impl StdoutStream for PluginLogStream {
    fn p2_stream(&self) -> Box<dyn OutputStream> {
        Box::new(self.clone())
    }

    fn async_stream(&self) -> Box<dyn AsyncWrite + Send + Sync> {
        Box::new(self.clone())
    }
}

#[wasmtime_wasi::async_trait]
impl Pollable for PluginLogStream {
    async fn ready(&mut self) {}
}

#[wasmtime_wasi::async_trait]
impl OutputStream for PluginLogStream {
    fn write(&mut self, bytes: Bytes) -> StreamResult<()> {
        self.push(&bytes);
        Ok(())
    }

    fn flush(&mut self) -> StreamResult<()> {
        Ok(())
    }

    fn check_write(&mut self) -> StreamResult<usize> {
        Ok(LINE_PERMIT)
    }
}

impl AsyncWrite for PluginLogStream {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.push(buf);
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lines are the unit: a write carrying two newlines logs two entries,
    /// and the tail after the last newline waits for its terminator.
    #[test]
    fn splits_writes_into_complete_lines() {
        let stream = PluginLogStream::new("p", LogLevel::Info, None, DatabaseBackend::Sqlite);
        let mut sink = stream.clone();
        sink.write(Bytes::from_static(b"one\ntwo\nthr")).unwrap();
        assert_eq!(stream.pending.lock().unwrap().as_slice(), b"thr");
        sink.write(Bytes::from_static(b"ee\n")).unwrap();
        assert!(stream.pending.lock().unwrap().is_empty());
    }

    #[test]
    fn an_unterminated_line_is_cut_at_the_permit() {
        let stream = PluginLogStream::new("p", LogLevel::Info, None, DatabaseBackend::Sqlite);
        let mut sink = stream.clone();
        sink.write(Bytes::from(vec![b'x'; LINE_PERMIT])).unwrap();
        assert!(stream.pending.lock().unwrap().is_empty());
    }

    #[test]
    fn the_context_has_no_environment_or_preopens() {
        // Building must not panic and must not consult the host process.
        let _ctx = build_context("p", None, DatabaseBackend::Sqlite);
    }
}
