//! Fails a test binary that hangs between one test and the next, rather than
//! letting it sit until CI's job timeout.
//!
//! A test that leaves work behind on a thread its runtime has to wait for
//! hangs after its body has passed, while the runtime is dropped, where no
//! timeout inside the test can see it. Every test after it in a `#[serial]`
//! queue then waits too. So the watchdog runs while no [`TestApp`] is alive,
//! from one dropped to the next built, and aborts the binary if that lasts
//! longer than [`LIMIT`] (or `HAPPYVIEW_TEST_WATCHDOG_SECS`).
//!
//! [`TestApp`]: super::app::TestApp

use std::io::Write as _;
use std::sync::{Mutex, Once};
use std::time::{Duration, Instant};

const LIMIT: Duration = Duration::from_secs(300);

/// How many [`Alive`] guards exist, and since when and after what there have
/// been none.
struct State {
    alive: usize,
    idle_since: Option<(Instant, String)>,
}

static STATE: Mutex<State> = Mutex::new(State {
    alive: 0,
    idle_since: None,
});
static START: Once = Once::new();

/// Held by every [`TestApp`](super::app::TestApp) from before it is built
/// until after the rest of it is dropped.
pub struct Alive;

impl Alive {
    pub fn new() -> Self {
        START.call_once(start);
        let mut state = lock();
        state.alive += 1;
        state.idle_since = None;
        Alive
    }
}

impl Drop for Alive {
    fn drop(&mut self) {
        let mut state = lock();
        state.alive -= 1;
        if state.alive == 0 {
            let test = std::thread::current()
                .name()
                .unwrap_or("an unnamed thread")
                .to_string();
            state.idle_since = Some((Instant::now(), test));
        }
    }
}

fn lock() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

fn limit() -> Duration {
    std::env::var("HAPPYVIEW_TEST_WATCHDOG_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .map(Duration::from_secs)
        .unwrap_or(LIMIT)
}

fn start() {
    let limit = limit();
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_secs(5));
            let state = lock();
            if let Some((since, test)) = &state.idle_since
                && since.elapsed() > limit
            {
                // Straight to stderr: `eprintln!` here would go to the
                // capture of the test that started this thread, which an
                // abort discards.
                let _ = writeln!(
                    std::io::stderr(),
                    "test watchdog: no test has started for {limit:?} since {test} \
                     dropped its TestApp; it probably hung while its runtime shut down. \
                     Aborting."
                );
                std::process::abort();
            }
        }
    });
}
