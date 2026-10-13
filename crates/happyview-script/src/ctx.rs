use std::rc::Rc;

use happyview_plugin_sdk::host;
use happyview_plugin_sdk::wire::Level;
use happyview_plugin_sdk::{
    ExecuteContext, ExecuteInput, JobProgressRequest, LibraryRef, Map, ScriptKind,
    ScriptLogRequest, Value,
};

pub use crate::executor::LibraryCall;
use crate::{Error, Result};

/// The run a handler is answering: who called, with what context, and the
/// libraries it can reach. Cheap to clone, so several joined calls can each
/// hold one.
#[derive(Clone)]
pub struct Ctx {
    inner: Rc<Inner>,
}

struct Inner {
    kind: ScriptKind,
    context: ExecuteContext,
    libraries: Vec<LibraryRef>,
}

impl Ctx {
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the module entry point the handler macros expand to is its only caller"
        )
    )]
    pub(crate) fn new(input: &ExecuteInput) -> Self {
        Self {
            inner: Rc::new(Inner {
                kind: input.kind,
                context: input.context.clone(),
                libraries: input.libraries.clone(),
            }),
        }
    }

    pub fn trigger(&self) -> &str {
        &self.inner.context.trigger
    }

    pub fn kind(&self) -> ScriptKind {
        self.inner.kind
    }

    pub fn caller_did(&self) -> Option<&str> {
        self.inner.context.caller_did.as_deref()
    }

    pub fn has_pds_auth(&self) -> bool {
        self.inner.context.has_pds_auth
    }

    pub fn env(&self, key: &str) -> Option<&str> {
        self.inner.context.env.get(key).map(String::as_str)
    }

    pub fn method(&self) -> Option<&str> {
        self.inner.context.method.as_deref()
    }

    pub fn collection(&self) -> Option<&str> {
        self.inner.context.collection.as_deref()
    }

    pub fn params(&self) -> Option<&Map<String, Value>> {
        self.inner.context.params.as_ref()
    }

    pub fn job_id(&self) -> Option<&str> {
        self.inner.context.job.as_ref().map(|job| job.id.as_str())
    }

    /// Everything the host sent about the run, for what the accessors above
    /// do not cover (a space, a delegate).
    pub fn context(&self) -> &ExecuteContext {
        &self.inner.context
    }

    /// Call `function` on the library installed under `namespace`. Nothing is
    /// sent until the future is first polled. Needs `library:call` in the
    /// module's `capabilities`.
    pub fn call(&self, namespace: &str, function: &str, args: Vec<Value>) -> LibraryCall {
        match self
            .inner
            .libraries
            .iter()
            .find(|lib| lib.namespace == namespace)
        {
            Some(lib) => LibraryCall::new(lib.id.clone(), function.to_string(), args),
            None => LibraryCall::failed(Error::msg(format!(
                "no library is installed under the namespace '{namespace}'"
            ))),
        }
    }

    /// A line in the run's log. Needs `script:host`.
    pub fn log(&self, level: Level, message: &str) -> Result<()> {
        host::script_log(&ScriptLogRequest {
            level,
            message: message.to_string(),
            fields: None,
        })
        .map_err(Error::from)
    }

    /// Store `data` as the running job's progress. Needs `script:host`.
    pub fn job_progress(&self, data: Value) -> Result<()> {
        host::job_progress(&JobProgressRequest { data }).map_err(Error::from)
    }

    /// Whether the running job has been paused or cancelled. Needs `script:host`.
    pub fn job_should_stop(&self) -> Result<bool> {
        host::job_should_stop().map_err(Error::from)
    }

    /// Sleep inside a job, between 0 and 3600 seconds. Needs `script:host`.
    pub fn job_wait(&self, seconds: f64) -> Result<()> {
        host::job_wait(seconds).map_err(Error::from)
    }
}
