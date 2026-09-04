//! Where a plugin's time went, split at the host/guest boundary.
//!
//! `PluginStats` answers "how much did this plugin consume"; this answers "and
//! where did it go". The two share a rationale — both are observed at the
//! crossing rather than reported by the guest — and ADR-0009 is the argument
//! for splitting the answer three ways rather than handing over a sampled
//! guest profile and calling it done.
//!
//! The split comes from [`wasmtime::CallHook`], which fires on every
//! host↔guest transition. `CallingWasm`/`ReturningFromWasm` bracket the whole
//! time spent inside `Func::call`'s wasm window; `CallingHost`/
//! `ReturningFromHost` bracket each host function invoked from within it. So:
//!
//! - **host call** is the sum of the inner windows,
//! - **guest** is the outer window minus the inner ones,
//! - **marshalling** is everything else, and it is a *remainder*.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use wasmtime::{CallHook, GuestProfiler};

/// Which side of the boundary a per-function row describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FunctionKind {
    /// A function the component exports, entered through [`crate::Plugin::call`].
    Export,
    /// A function the host serves and the component imported.
    Import,
}

/// Nanosecond totals for one caller-visible unit of work.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Totals {
    calls: u64,
    wall_nanos: u64,
    guest_nanos: u64,
    host_nanos: u64,
}

impl Totals {
    fn add(&mut self, wall: u64, guest: u64, host: u64) {
        self.calls += 1;
        self.wall_nanos = self.wall_nanos.saturating_add(wall);
        self.guest_nanos = self.guest_nanos.saturating_add(guest);
        self.host_nanos = self.host_nanos.saturating_add(host);
    }

    /// What is left of the wall time once the two measured buckets are taken
    /// out. See [`PluginProfile::marshalling_nanos`] for why this is not a
    /// measurement.
    fn marshalling(self) -> u64 {
        self.wall_nanos
            .saturating_sub(self.guest_nanos)
            .saturating_sub(self.host_nanos)
    }
}

/// One row of per-WIT-function attribution.
///
/// An **export** row covers one call through [`crate::Plugin::call`] and
/// carries all three buckets. An **import** row covers the host function the
/// guest called: `host_nanos` is the time spent inside it and `wall_nanos`
/// equals it, because a host function has no boundary of its own that watoots
/// can see from here — `guest_nanos` and `marshalling_nanos` are zero.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionProfile {
    /// Whether this is an export the host called or an import the guest did.
    pub kind: FunctionKind,
    /// The interface the function belongs to, version included. Empty for an
    /// export, which [`crate::Plugin::call`] names without one.
    pub interface: String,
    /// The function's own name.
    pub func: String,
    /// How many times it was entered.
    pub calls: u64,
    /// Wall time across those entries.
    pub wall_nanos: u64,
    /// Time inside wasm, host calls made from it excluded.
    pub guest_nanos: u64,
    /// Time inside host functions.
    pub host_nanos: u64,
    /// The remainder. See [`PluginProfile::marshalling_nanos`].
    pub marshalling_nanos: u64,
}

/// Where a plugin's time went since it was loaded.
///
/// Every number is observed at the boundary, so the guest can neither forge
/// nor inflate it — the same property that makes [`crate::PluginStats`] worth
/// having (ADR-0006).
///
/// # What the buckets do and do not cover
///
/// `guest_nanos` and `host_nanos` are *measured*, from the exact transitions
/// wasmtime reports. `marshalling_nanos` is **derived**: it is
/// `wall_nanos - guest_nanos - host_nanos` and nothing observes it directly.
/// The canonical ABI's lift and lower do land there, which is the question it
/// exists to answer — "is my time going into copying?" — but so does every
/// other cost of getting into and out of a call, watoots' own dispatch
/// included. Read it as "not accounted for by the other two", not as a
/// measurement of copying.
///
/// Two more caveats worth knowing before drawing a conclusion from a number:
///
/// - `host_nanos` counts *every* host call, including the `wasi:` interfaces
///   `wasmtime-wasi` implements and any runtime libcall that leaves wasm. Only
///   the host functions watoots installed itself get a [`FunctionProfile`] row,
///   so the rows will not add up to the bucket.
/// - Profiling changes timing. A profiled run is not a determinism-preserving
///   run, which is why enabling it alongside a trace recorder is refused rather
///   than silently permitted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginProfile {
    /// Calls through [`crate::Plugin::call`] that have completed.
    pub calls: u64,
    /// Wall time spent inside those calls.
    pub wall_nanos: u64,
    /// Of that, time executing wasm, excluding host calls made from it.
    pub guest_nanos: u64,
    /// Of that, time inside host functions the guest called.
    pub host_nanos: u64,
    /// What is left: the canonical ABI's lift and lower, plus watoots' own
    /// dispatch. A remainder, not a measurement — see the type documentation.
    pub marshalling_nanos: u64,
    /// Per-function attribution, exports first, each group sorted by name.
    pub functions: Vec<FunctionProfile>,
}

/// How a host profiles the plugins it loads.
///
/// Off by default and opt-in per host: profiling costs a call hook on every
/// crossing, and asking for guest samples costs an epoch deadline the timeout
/// then has to share.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Profiling {
    /// Sample guest stacks at this interval for a Firefox Profiler JSON, or
    /// `None` to collect the boundary buckets only.
    ///
    /// Sampling is driven from the epoch deadline, which also enforces
    /// `limits.timeout`, so an interval is rounded up to whole epoch ticks
    /// (1ms) and **never extends a plugin's life**: the callback continues with
    /// `min(interval, remaining timeout)` and traps when the budget is spent.
    pub sample_interval: Option<Duration>,
}

/// The per-call epoch budget, tracked in ticks.
///
/// Both `limits.timeout` and guest sampling want the epoch deadline, and
/// whichever sets it last wins — so the remaining timeout is tracked here
/// rather than inferred from the deadline. The timeout is authoritative:
/// [`Deadline::next`] hands out `min(sample, remaining)` and returns `None`
/// once the budget is gone, which the callback turns into a trap.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Deadline {
    /// The per-call timeout in epoch ticks, or `None` when the manifest sets
    /// none.
    budget: Option<u64>,
    /// The sampling interval in epoch ticks, or `None` when sampling is off.
    sample: Option<u64>,
    /// Ticks of `budget` not yet handed to the epoch deadline.
    remaining: u64,
}

impl Deadline {
    pub(crate) fn new(budget: Option<u64>, sample: Option<u64>) -> Self {
        Self {
            budget,
            sample,
            remaining: 0,
        }
    }

    /// Whether anything wants an epoch deadline at all.
    pub(crate) fn is_armed(self) -> bool {
        self.budget.is_some() || self.sample.is_some()
    }

    /// Whether the epoch deadline has to be shared, and so needs a callback.
    pub(crate) fn samples(self) -> bool {
        self.sample.is_some()
    }

    /// Reset for a new call and return the first slice, in ticks.
    pub(crate) fn rearm(&mut self) -> Option<u64> {
        // No timeout means an inexhaustible budget rather than a large one:
        // `next` only draws down when `budget` is set.
        self.remaining = self.budget.unwrap_or(u64::MAX);
        self.next()
    }

    /// The next slice, or `None` when the timeout has been spent.
    pub(crate) fn next(&mut self) -> Option<u64> {
        if !self.is_armed() || self.remaining == 0 {
            return None;
        }
        let step = self
            .sample
            .map_or(self.remaining, |s| s.min(self.remaining));
        if self.budget.is_some() {
            self.remaining -= step;
        }
        // A deadline of zero ticks fires immediately and forever; one tick is
        // the floor the epoch granularity can express.
        Some(step.max(1))
    }
}

/// Everything the profiler accumulates, living in the plugin's store so the
/// call hook and the epoch callback can both reach it.
pub(crate) struct ProfileState {
    /// The Firefox-profile sampler, when one was asked for. Taken out by
    /// [`crate::Plugin::write_guest_profile`], which is why it is an `Option`
    /// beyond "sampling is off".
    pub(crate) guest: Option<GuestProfiler>,
    /// What [`GuestProfiler::sample`] is told each sample covered.
    last_sample: Option<Instant>,

    wasm_depth: u32,
    wasm_since: Option<Instant>,
    wasm_nanos: u64,

    host_depth: u32,
    host_since: Option<Instant>,
    host_nanos: u64,
    /// The import the open host window is serving, set by the shim once it is
    /// entered. `None` for a `wasi:` interface or a libcall, whose time still
    /// lands in the host bucket but gets no row.
    serving: Option<(String, String)>,

    total: Totals,
    exports: BTreeMap<String, Totals>,
    imports: BTreeMap<(String, String), Totals>,
}

impl std::fmt::Debug for ProfileState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProfileState")
            .field("sampling", &self.guest.is_some())
            .field("total", &self.total)
            .finish_non_exhaustive()
    }
}

impl ProfileState {
    pub(crate) fn new(guest: Option<GuestProfiler>) -> Self {
        Self {
            guest,
            last_sample: None,
            wasm_depth: 0,
            wasm_since: None,
            wasm_nanos: 0,
            host_depth: 0,
            host_since: None,
            host_nanos: 0,
            serving: None,
            total: Totals::default(),
            exports: BTreeMap::new(),
            imports: BTreeMap::new(),
        }
    }

    /// Record one boundary transition.
    ///
    /// The wasm window nests: a host function that called back into the guest
    /// would leave its own window open around that re-entry, and the inner wasm
    /// time would be counted as host time. watoots installs no such host
    /// function, and the depth counters keep the arithmetic sane rather than
    /// exact if one ever appears.
    pub(crate) fn on_call_hook(&mut self, kind: CallHook) {
        match kind {
            CallHook::CallingWasm => {
                if self.wasm_depth == 0 {
                    self.wasm_since = Some(Instant::now());
                }
                self.wasm_depth += 1;
            }
            CallHook::ReturningFromWasm => {
                self.wasm_depth = self.wasm_depth.saturating_sub(1);
                if self.wasm_depth == 0
                    && let Some(since) = self.wasm_since.take()
                {
                    self.wasm_nanos = self.wasm_nanos.saturating_add(nanos(since.elapsed()));
                }
            }
            CallHook::CallingHost => {
                if self.host_depth == 0 {
                    self.host_since = Some(Instant::now());
                    // Cleared here rather than on the way out, so a WASI call
                    // following an application one does not inherit its label.
                    self.serving = None;
                }
                self.host_depth += 1;
            }
            CallHook::ReturningFromHost => {
                self.host_depth = self.host_depth.saturating_sub(1);
                if self.host_depth == 0
                    && let Some(since) = self.host_since.take()
                {
                    let spent = nanos(since.elapsed());
                    self.host_nanos = self.host_nanos.saturating_add(spent);
                    if let Some(key) = self.serving.take() {
                        self.imports.entry(key).or_default().add(spent, 0, spent);
                    }
                }
            }
        }
    }

    /// Name the import whose host window is currently open.
    ///
    /// Called from the shim itself, which is the only place that knows which
    /// interface and function it serves — the call hook sees a transition, not
    /// a name.
    pub(crate) fn serving(&mut self, interface: &str, func: &str) {
        self.serving = Some((interface.to_string(), func.to_string()));
    }

    /// How long since the previous sample, and reset the clock.
    pub(crate) fn sample_delta(&mut self) -> Duration {
        let now = Instant::now();
        let delta = self.last_sample.map_or(Duration::ZERO, |at| now - at);
        self.last_sample = Some(now);
        delta
    }

    /// Fold the call that just finished into the totals.
    pub(crate) fn finish_call(&mut self, export: &str, wall: Duration) {
        let wall = nanos(wall);
        let host = self.host_nanos;
        let guest = self.wasm_nanos.saturating_sub(host);

        self.total.add(wall, guest, host);
        self.exports
            .entry(export.to_string())
            .or_default()
            .add(wall, guest, host);

        // Per call, like fuel and the deadline. The lifetime totals above are
        // not.
        self.wasm_depth = 0;
        self.wasm_since = None;
        self.wasm_nanos = 0;
        self.host_depth = 0;
        self.host_since = None;
        self.host_nanos = 0;
        self.serving = None;
    }

    /// The answer `Plugin::profile` hands out.
    pub(crate) fn report(&self) -> PluginProfile {
        let mut functions: Vec<FunctionProfile> =
            Vec::with_capacity(self.exports.len() + self.imports.len());
        for (func, totals) in &self.exports {
            functions.push(FunctionProfile {
                kind: FunctionKind::Export,
                interface: String::new(),
                func: func.clone(),
                calls: totals.calls,
                wall_nanos: totals.wall_nanos,
                guest_nanos: totals.guest_nanos,
                host_nanos: totals.host_nanos,
                marshalling_nanos: totals.marshalling(),
            });
        }
        for ((interface, func), totals) in &self.imports {
            functions.push(FunctionProfile {
                kind: FunctionKind::Import,
                interface: interface.clone(),
                func: func.clone(),
                calls: totals.calls,
                wall_nanos: totals.wall_nanos,
                guest_nanos: 0,
                host_nanos: totals.host_nanos,
                marshalling_nanos: 0,
            });
        }

        PluginProfile {
            calls: self.total.calls,
            wall_nanos: self.total.wall_nanos,
            guest_nanos: self.total.guest_nanos,
            host_nanos: self.total.host_nanos,
            marshalling_nanos: self.total.marshalling(),
            functions,
        }
    }
}

/// Saturating rather than wrapping: 584 years of one call is not a number
/// anyone needs, and a silently negative one is worse than a clamped one.
fn nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unarmed_deadline_asks_for_nothing() {
        let mut deadline = Deadline::new(None, None);
        assert!(!deadline.is_armed());
        assert_eq!(deadline.rearm(), None);
    }

    #[test]
    fn a_timeout_without_sampling_is_handed_out_whole() {
        let mut deadline = Deadline::new(Some(200), None);
        assert_eq!(deadline.rearm(), Some(200));
        // And then it is spent, which the callback turns into a trap.
        assert_eq!(deadline.next(), None);
    }

    #[test]
    fn sampling_never_extends_the_budget() {
        // Ten ticks of budget, sampled every three: the slices have to add up
        // to ten and then stop, however many times the callback fires.
        let mut deadline = Deadline::new(Some(10), Some(3));
        let mut handed = 0;
        let mut slice = deadline.rearm();
        while let Some(ticks) = slice {
            handed += ticks;
            assert!(
                handed <= 10,
                "handed out {handed} ticks of a 10 tick budget"
            );
            slice = deadline.next();
        }
        assert_eq!(handed, 10);
    }

    #[test]
    fn a_sample_longer_than_the_budget_does_not_overshoot() {
        let mut deadline = Deadline::new(Some(5), Some(1000));
        assert_eq!(deadline.rearm(), Some(5), "the timeout is the shorter one");
        assert_eq!(deadline.next(), None);
    }

    #[test]
    fn sampling_without_a_timeout_runs_forever() {
        let mut deadline = Deadline::new(None, Some(7));
        assert_eq!(deadline.rearm(), Some(7));
        for _ in 0..1000 {
            assert_eq!(deadline.next(), Some(7));
        }
    }

    #[test]
    fn rearming_restores_the_whole_budget() {
        let mut deadline = Deadline::new(Some(4), Some(2));
        assert_eq!(deadline.rearm(), Some(2));
        assert_eq!(deadline.next(), Some(2));
        assert_eq!(deadline.next(), None);
        assert_eq!(deadline.rearm(), Some(2), "a new call gets the full budget");
    }

    #[test]
    fn marshalling_is_the_remainder() {
        let mut totals = Totals::default();
        totals.add(100, 60, 30);
        assert_eq!(totals.marshalling(), 10);
    }

    #[test]
    fn a_remainder_never_goes_negative() {
        // Clock skew between the wall measurement and the hook measurements
        // must not produce a nonsense number.
        let mut totals = Totals::default();
        totals.add(10, 60, 30);
        assert_eq!(totals.marshalling(), 0);
    }
}
