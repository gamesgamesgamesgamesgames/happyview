//! The PDS a test run talks to instead of the network.

use std::sync::Mutex;

use crate::plugin::TestPdsWrite;

#[derive(Debug, Default)]
pub struct FakePds {
    writes: Mutex<Vec<TestPdsWrite>>,
}

impl FakePds {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every write accepted since the last reported test, in order.
    pub fn writes(&self) -> Vec<TestPdsWrite> {
        self.writes.lock().unwrap().clone()
    }

    pub(crate) fn clear(&self) {
        self.writes.lock().unwrap().clear();
    }
}
