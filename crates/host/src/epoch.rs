//! The engine epoch: the clock every per-call deadline is measured against.
//!
//! Wasmtime interrupts a running guest when the engine's epoch passes the
//! store's deadline. Somebody has to advance the epoch; this is that somebody,
//! plus the arithmetic that turns a `Duration` into ticks of it.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use wasmtime::Engine;

/// How often the epoch ticker advances the engine's epoch.
///
/// Deadlines are expressed in whole ticks, so this is also the granularity of a
/// `timeout`: a 200ms timeout is 200 ticks.
const EPOCH_TICK: Duration = Duration::from_millis(1);

/// How many epoch ticks a duration is worth, rounded up and never zero.
///
/// A deadline of zero ticks has already expired, so anything shorter than one
/// tick becomes one tick: that is the granularity `EPOCH_TICK` can express, and
/// asking for less would trap immediately rather than sooner.
pub(crate) fn epoch_ticks(duration: Duration) -> u64 {
    let tick = EPOCH_TICK.as_nanos();
    u64::try_from(duration.as_nanos().div_ceil(tick))
        .unwrap_or(u64::MAX)
        .max(1)
}

/// Advances the engine epoch on a fixed tick so per-call deadlines can fire.
pub(crate) struct EpochTicker {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl EpochTicker {
    pub(crate) fn spawn(engine: Engine) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let handle = std::thread::Builder::new()
            .name("watoots-epoch".to_string())
            .spawn(move || {
                while !flag.load(Ordering::Relaxed) {
                    std::thread::sleep(EPOCH_TICK);
                    engine.increment_epoch();
                }
            })
            .expect("spawning the epoch ticker");

        Self {
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for EpochTicker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}
