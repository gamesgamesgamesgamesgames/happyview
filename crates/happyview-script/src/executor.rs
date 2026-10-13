//! Drives a handler's future to completion on the guest's one thread.
//!
//! The only thing a handler can wait on is a library call, so the executor
//! needs no wakers: it polls the handler until it is pending, hands every
//! started call to `host_call_library_wait_any`, files the one that settled,
//! and polls again. A call is started on its first poll, which is why
//! `join!` starts every call it holds before the first wait.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::{pin, Pin};
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

use happyview_plugin_sdk::{host, PluginError, Value};

use crate::{Error, Result};

type Settled = std::result::Result<Value, PluginError>;

pub(crate) trait Host {
    fn start(
        &self,
        library: &str,
        function: &str,
        args: &[Value],
    ) -> std::result::Result<u32, PluginError>;
    fn wait_any(&self, handles: &[u32]) -> std::result::Result<(u32, Settled), PluginError>;
}

struct Wasm;

impl Host for Wasm {
    fn start(
        &self,
        library: &str,
        function: &str,
        args: &[Value],
    ) -> std::result::Result<u32, PluginError> {
        host::call_library_start(library, function, args)
    }

    fn wait_any(&self, handles: &[u32]) -> std::result::Result<(u32, Settled), PluginError> {
        host::call_library_wait_any(handles)
    }
}

thread_local! {
    static HOST: RefCell<Rc<dyn Host>> = RefCell::new(Rc::new(Wasm));
    /// Started calls nothing has collected yet.
    static WAITING: RefCell<BTreeSet<u32>> = const { RefCell::new(BTreeSet::new()) };
    /// Settled calls whose future has not been polled since.
    static SETTLED: RefCell<BTreeMap<u32, Settled>> = const { RefCell::new(BTreeMap::new()) };
}

fn current_host() -> Rc<dyn Host> {
    HOST.with(|h| h.borrow().clone())
}

/// Replace the host and forget every call, for a native test.
#[cfg(test)]
pub(crate) fn with_host(host: Rc<dyn Host>) {
    HOST.with(|h| *h.borrow_mut() = host);
    WAITING.with(|w| w.borrow_mut().clear());
    SETTLED.with(|s| s.borrow_mut().clear());
}

/// A library call. Nothing is sent until it is first polled.
pub struct LibraryCall {
    state: CallState,
}

enum CallState {
    Unstarted {
        library: String,
        function: String,
        args: Vec<Value>,
    },
    Started(u32),
    Failed(Error),
    Finished,
}

impl LibraryCall {
    pub(crate) fn new(library: String, function: String, args: Vec<Value>) -> Self {
        Self {
            state: CallState::Unstarted {
                library,
                function,
                args,
            },
        }
    }

    pub(crate) fn failed(error: Error) -> Self {
        Self {
            state: CallState::Failed(error),
        }
    }
}

impl Future for LibraryCall {
    type Output = Result<Value>;

    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        match std::mem::replace(&mut this.state, CallState::Finished) {
            CallState::Unstarted {
                library,
                function,
                args,
            } => match current_host().start(&library, &function, &args) {
                Ok(handle) => {
                    WAITING.with(|w| w.borrow_mut().insert(handle));
                    this.state = CallState::Started(handle);
                    Poll::Pending
                }
                Err(e) => Poll::Ready(Err(e.into())),
            },
            CallState::Started(handle) => match SETTLED.with(|s| s.borrow_mut().remove(&handle)) {
                Some(result) => Poll::Ready(result.map_err(Error::from)),
                None => {
                    this.state = CallState::Started(handle);
                    Poll::Pending
                }
            },
            CallState::Failed(e) => Poll::Ready(Err(e)),
            CallState::Finished => panic!("a library call was polled after it finished"),
        }
    }
}

impl Drop for LibraryCall {
    /// An abandoned call is left to the host, which drains it when the run
    /// answers; waiting on it here would only delay the handler.
    fn drop(&mut self) {
        if let CallState::Started(handle) = self.state {
            WAITING.with(|w| w.borrow_mut().remove(&handle));
            SETTLED.with(|s| s.borrow_mut().remove(&handle));
        }
    }
}

pub(crate) fn block_on<F: Future>(fut: F) -> Result<F::Output> {
    let mut fut = pin!(fut);
    let mut cx = Context::from_waker(Waker::noop());
    loop {
        if let Poll::Ready(output) = fut.as_mut().poll(&mut cx) {
            return Ok(output);
        }
        let handles: Vec<u32> = WAITING.with(|w| w.borrow().iter().copied().collect());
        if handles.is_empty() {
            return Err(Error::msg(
                "the handler is waiting on something other than a library call, \
                 and nothing in a script module can wake it",
            ));
        }
        let (handle, result) = current_host().wait_any(&handles)?;
        WAITING.with(|w| w.borrow_mut().remove(&handle));
        SETTLED.with(|s| s.borrow_mut().insert(handle, result));
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::Ctx;
    use happyview_plugin_sdk::{
        ExecuteContext, ExecuteInput, ExecuteLimits, LibraryRef, ScriptKind, DEFAULT_CHUNK_NAME,
    };
    use serde_json::json;
    use std::cell::RefCell;

    /// A host that records what it was asked and settles calls in the order
    /// they were started, answering each with its own function name.
    #[derive(Default)]
    pub(crate) struct FakeHost {
        pub log: RefCell<Vec<String>>,
        next: RefCell<u32>,
        started: RefCell<Vec<(u32, String)>>,
    }

    impl Host for FakeHost {
        fn start(
            &self,
            library: &str,
            function: &str,
            _args: &[Value],
        ) -> std::result::Result<u32, PluginError> {
            let mut next = self.next.borrow_mut();
            *next += 1;
            self.log
                .borrow_mut()
                .push(format!("start {library}.{function} -> {next}"));
            self.started
                .borrow_mut()
                .push((*next, function.to_string()));
            Ok(*next)
        }

        fn wait_any(&self, handles: &[u32]) -> std::result::Result<(u32, Settled), PluginError> {
            self.log.borrow_mut().push(format!("wait {handles:?}"));
            let started = self.started.borrow();
            let (handle, function) = started
                .iter()
                .find(|(h, _)| handles.contains(h))
                .cloned()
                .ok_or_else(|| PluginError::new("BAD_INPUT", "no such handle"))?;
            Ok((handle, Ok(json!(function))))
        }
    }

    pub(crate) fn ctx() -> Ctx {
        Ctx::new(&ExecuteInput {
            source: String::new(),
            kind: ScriptKind::XrpcQuery,
            input: Value::Null,
            context: ExecuteContext {
                trigger: "xrpc.query:app.test.q".into(),
                ..Default::default()
            },
            libraries: vec![LibraryRef {
                namespace: "http".into(),
                id: "sdk_http".into(),
            }],
            limits: ExecuteLimits {
                instructions: Some(1_000_000),
                memory_bytes: 1 << 24,
            },
            removed_globals: Vec::new(),
            chunk_name: DEFAULT_CHUNK_NAME.into(),
            artifact: None,
        })
    }

    #[test]
    fn joined_calls_all_start_before_the_first_wait() {
        let host = Rc::new(FakeHost::default());
        with_host(host.clone());
        let ctx = ctx();
        let (a, b) = block_on(async {
            crate::join!(ctx.call("http", "a", vec![]), ctx.call("http", "b", vec![]))
        })
        .unwrap();
        assert_eq!(a.unwrap(), json!("a"));
        assert_eq!(b.unwrap(), json!("b"));
        assert_eq!(
            *host.log.borrow(),
            vec![
                "start sdk_http.a -> 1",
                "start sdk_http.b -> 2",
                "wait [1, 2]",
                "wait [2]",
            ]
        );
    }

    #[test]
    fn a_sequential_await_waits_on_one_call_at_a_time() {
        let host = Rc::new(FakeHost::default());
        with_host(host.clone());
        let ctx = ctx();
        block_on(async {
            ctx.call("http", "a", vec![]).await.unwrap();
            ctx.call("http", "b", vec![]).await.unwrap();
        })
        .unwrap();
        assert_eq!(
            *host.log.borrow(),
            vec![
                "start sdk_http.a -> 1",
                "wait [1]",
                "start sdk_http.b -> 2",
                "wait [2]"
            ]
        );
    }

    #[test]
    fn an_unknown_namespace_fails_without_reaching_the_host() {
        let host = Rc::new(FakeHost::default());
        with_host(host.clone());
        let err = block_on(ctx().call("nope", "a", vec![]))
            .unwrap()
            .unwrap_err();
        assert!(err.message().contains("'nope'"), "{err}");
        assert!(host.log.borrow().is_empty());
    }

    #[test]
    fn waiting_on_something_no_library_call_wakes_is_an_error_not_a_hang() {
        with_host(Rc::new(FakeHost::default()));
        let err = block_on(std::future::pending::<()>()).unwrap_err();
        assert!(err.message().contains("other than a library call"), "{err}");
    }

    #[test]
    fn a_dropped_call_is_not_waited_on() {
        let host = Rc::new(FakeHost::default());
        with_host(host.clone());
        let ctx = ctx();
        block_on(async {
            // Boxed, so dropping it drops the call rather than a pin of it.
            let mut first = Box::pin(ctx.call("http", "a", vec![]));
            // Start it, then abandon it.
            let _ = futures_util::poll!(first.as_mut());
            drop(first);
            ctx.call("http", "b", vec![]).await.unwrap();
        })
        .unwrap();
        assert_eq!(
            *host.log.borrow(),
            vec!["start sdk_http.a -> 1", "start sdk_http.b -> 2", "wait [2]"]
        );
    }
}
