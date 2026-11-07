//! Sizing the echo fixture's `burn:` directive from the machine it runs on.
//!
//! A deadline that bounds guest CPU can only be told apart from a lifted one
//! by a burn that outlasts the budget *and then returns*, so a burn too short
//! for the budget proves nothing and a burn far longer than it costs a serial
//! suite seconds. One iteration count cannot be both on two machines: the one
//! that measured it pays the seconds, and anything faster outruns the budget
//! the count was sized against. The count is therefore measured here, on every
//! run, against the fixture that will spend it.
//!
//! The fixture cannot bound itself instead — it is built for
//! `wasm32-unknown-unknown`, so it imports no clock to read.

use std::time::{Duration, Instant};

use happyview::AppState;
use happyview::plugin::{
    ExecutionError, ScriptExecuteContext, ScriptExecuteInput, ScriptExecuteLimits, ScriptKind,
};
use serde_json::Value;

/// What a burn is sized to cost: three times the one-second budget a caller
/// sets, so the margin absorbs the error in a measured rate.
pub const TARGET: Duration = Duration::from_secs(3);

/// The shortest reading a rate may be derived from. Under this a scheduling
/// stall is a large fraction of it, so the sample grows until it clears this
/// rather than starting at a size some machine's speed was assumed for.
const MIN_READING: Duration = Duration::from_millis(40);

/// Where the sample starts, how fast it grows, and where growing it stops
/// meaning anything. The start is short enough to cost little on a slow
/// machine; growing it eightfold reaches a usable reading on a fast one in a
/// step or two, and the discarded steps together cost a fraction of the one
/// that is kept. No machine runs a trillion iterations inside the target
/// above, so reaching the ceiling says the directive spends nothing — the
/// fixture built for the host target rather than wasm does exactly that —
/// and growing further would only overflow.
const FIRST_SAMPLE: u64 = 10_000_000;
const GROWTH: u64 = 8;
const MAX_SAMPLE: u64 = 1_000_000_000_000;

/// How many readings the rate is the fastest of. A stall inflates a reading
/// and so understates the rate, which would undersize the burn and leave it
/// short of the budget; the fastest of several discards one.
const READINGS: usize = 3;

/// A burn sized for this machine.
pub struct Burn {
    pub iterations: u64,
    /// Iterations of guest CPU per second, as measured.
    pub rate: f64,
}

impl Burn {
    /// The directive that spends it.
    pub fn source(&self) -> String {
        format!("burn:{}", self.iterations)
    }

    /// Panics unless `measured` outlasts `budget`. A caller's assertion that
    /// the run completed means nothing on its own, because a burn the guest
    /// never spent completes under a lifted deadline and an armed one alike.
    ///
    /// The bound is the budget, not [`TARGET`]: the burn was sized from a
    /// rate read a moment earlier, and a runner whose load eased since spends
    /// it faster, which is no failure of the deadline. [`Self::assert_bounded`]
    /// is what shows the burn outlasts the budget under this run's load.
    pub fn assert_outlasted(&self, measured: Duration, budget: Duration) {
        assert!(
            measured > budget,
            "a burn of {} iterations, sized for {TARGET:?} from a measured \
             {:.3e} iterations per second, took {measured:?}, within the \
             {budget:?} budget: the guest did not spend it, so the run \
             completing says nothing about the deadline it ran under",
            self.iterations,
            self.rate,
        );
    }

    /// Panics unless the burn, run as an XRPC query under `state`'s wall
    /// clock, times out: the burn outlasts that budget on this machine as it
    /// runs now, whatever the rate it was sized from.
    pub async fn assert_bounded(&self, state: &AppState, interpreter: &str) {
        let mut query = burn_input(&self.source(), ScriptKind::XrpcQuery);
        query.context.trigger = "xrpc.query:burn.bounded".into();
        let err = state
            .plugin_executor()
            .execute_script(interpreter, &query)
            .await
            .expect_err("a burn that outlasts the wall clock must not return from a query");
        assert!(matches!(err, ExecutionError::Timeout), "{err}");
    }
}

/// Measure how fast `interpreter` spends `burn:` iterations on this machine
/// and size one burn to [`TARGET`].
pub async fn calibrate(state: &AppState, interpreter: &str) -> Burn {
    // The first run of a module compiles it, which charged to a burn reads as
    // a machine two orders of magnitude slower than it is.
    sample(state, interpreter, 0).await;

    let mut iterations = FIRST_SAMPLE;
    let mut fastest = loop {
        let reading = sample(state, interpreter, iterations).await;
        if reading >= MIN_READING {
            break reading;
        }
        assert!(
            iterations < MAX_SAMPLE,
            "{iterations} iterations of `burn:` cost {reading:?}, under {MIN_READING:?}: \
             the directive is spending no guest CPU"
        );
        iterations *= GROWTH;
    };
    for _ in 1..READINGS {
        fastest = fastest.min(sample(state, interpreter, iterations).await);
    }

    let rate = iterations as f64 / fastest.as_secs_f64();
    Burn {
        iterations: (rate * TARGET.as_secs_f64()) as u64,
        rate,
    }
}

/// What one burn of `iterations` costs, measured as a job so no deadline can
/// truncate the reading.
async fn sample(state: &AppState, interpreter: &str, iterations: u64) -> Duration {
    let input = burn_input(&format!("burn:{iterations}"), ScriptKind::Job);

    let started = Instant::now();
    state
        .plugin_executor()
        .execute_script(interpreter, &input)
        .await
        .expect("a calibration burn must run");
    started.elapsed()
}

/// A run of `source` as `kind`.
fn burn_input(source: &str, kind: ScriptKind) -> ScriptExecuteInput {
    ScriptExecuteInput {
        source: source.to_string(),
        kind,
        input: Value::Null,
        context: ScriptExecuteContext {
            trigger: "job.run:burn.calibrate".into(),
            ..ScriptExecuteContext::default()
        },
        libraries: Vec::new(),
        limits: ScriptExecuteLimits {
            instructions: Some(1_000_000),
            memory_bytes: 16 * 1024 * 1024,
        },
        removed_globals: Vec::new(),
        chunk_name: happyview::plugin::DEFAULT_CHUNK_NAME.to_string(),
        artifact: None,
    }
}
