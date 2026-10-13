//! What one test file's run carries: its identity, its PDS and everything it
//! has observed and reported.

use std::sync::{Arc, Mutex};

use crate::plugin::{TestCall, TestReport};

use super::fake_pds::FakePds;

#[derive(Debug)]
pub struct TestHarness {
    file: String,
    pds: Arc<FakePds>,
    calls: Mutex<Vec<(String, TestCall)>>,
    reports: Mutex<Vec<TestReport>>,
    failures: Mutex<Vec<String>>,
}

impl TestHarness {
    pub fn new(file: impl Into<String>) -> Self {
        Self {
            file: file.into(),
            pds: Arc::new(FakePds::new()),
            calls: Mutex::new(Vec::new()),
            reports: Mutex::new(Vec::new()),
            failures: Mutex::new(Vec::new()),
        }
    }

    /// The test file's project-relative path, which is also its key in the
    /// snapshot and its chunk name.
    pub fn file(&self) -> &str {
        &self.file
    }

    pub fn pds(&self) -> &Arc<FakePds> {
        &self.pds
    }

    /// File one test's outcome. The path is the file this harness runs, not
    /// what the guest said, so a report cannot land under another file.
    ///
    /// Calls and PDS writes are what a test observes, so each test starts
    /// with none: they are cleared here, at the end of the previous one.
    pub fn report(&self, mut report: TestReport) {
        report.path = self.file.clone();
        self.reports.lock().unwrap().push(report);
        self.calls.lock().unwrap().clear();
        self.pds.clear();
    }

    pub fn reports(&self) -> Vec<TestReport> {
        self.reports.lock().unwrap().clone()
    }

    pub fn record_call(&self, namespace: &str, call: TestCall) {
        self.calls
            .lock()
            .unwrap()
            .push((namespace.to_string(), call));
    }

    pub fn calls(&self, namespace: &str) -> Vec<TestCall> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(ns, _)| ns == namespace)
            .map(|(_, call)| call.clone())
            .collect()
    }

    /// A record or label script that failed. Those runners are fail-open and
    /// only dead-letter, so this is how a trigger learns its script failed.
    pub fn note_script_failure(&self, error: &str) {
        self.failures.lock().unwrap().push(error.to_string());
    }

    pub fn take_script_failures(&self) -> Vec<String> {
        std::mem::take(&mut *self.failures.lock().unwrap())
    }
}

/// The caller an in-process request is forged as. An extension rather than a
/// header because nothing arriving over a socket can carry one, and
/// `XrpcClaims` honours it only on a state that has a harness.
#[derive(Debug, Clone)]
pub struct ForgedCaller(String);

impl ForgedCaller {
    pub fn new(did: impl Into<String>) -> Self {
        Self(did.into())
    }

    pub fn did(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TestHostError {
    #[error("{0}")]
    BadInput(String),
    #[error("{0}")]
    Host(String),
}

impl TestHostError {
    /// A malformed request is the test's to fix; anything else is the host.
    pub fn code(&self) -> &'static str {
        match self {
            Self::BadInput(_) => "BAD_INPUT",
            Self::Host(_) => "HOST_ERROR",
        }
    }
}
