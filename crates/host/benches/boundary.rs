//! What a plugin call actually costs.
//!
//! The questions a host author asks before adopting this, and nothing else:
//! what does one crossing cost, what do the safety knobs add, and what does the
//! dynamic path cost over the typed one. Every case runs the *same* trivial
//! guest, so the number is boundary overhead rather than a measure of somebody's
//! lint rules.
//!
//! Numbers are per machine and go stale; `docs/PERFORMANCE.md` records a run
//! with the machine named, and says what to conclude and what not to.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use watoots::{Host, Manifest, Plugin, Val};

/// Does as close to nothing as a component can, so the measurement is the
/// crossing and not the guest.
const NOOP: &str = r#"
(component
  (core module $m (func (export "answer") (result i32) (i32.const 42)))
  (core instance $i (instantiate $m))
  (func $answer (result s32) (canon lift (core func $i "answer")))
  (export "answer" (func $answer))
)
"#;

/// Takes and returns a string, so the canonical ABI has to lift and lower
/// rather than pass an integer in a register.
const ECHO: &str = r#"
(component
  (core module $m
    (memory (export "memory") 1)
    (func (export "echo") (param i32 i32) (result i32)
      ;; Return a pointer to a 2-word (ptr, len) pair describing the input,
      ;; which is already in memory. The host copies it back out.
      (i32.store (i32.const 1024) (local.get 0))
      (i32.store (i32.const 1028) (local.get 1))
      (i32.const 1024))
    (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32)
      (i32.const 16)))
  (core instance $i (instantiate $m))
  (func $echo (param "s" string) (result string)
    (canon lift (core func $i "echo")
      (memory $i "memory") (realloc (func $i "cabi_realloc"))))
  (export "echo" (func $echo))
)
"#;

/// Spins for a fixed number of iterations, so per-instruction costs have
/// something to act on.
///
/// The trivial guest cannot answer "what does fuel cost": fuel is metered per
/// instruction and `NOOP` executes one, so metering it is lost in the noise of
/// the crossing. The first version of this file reported fuel as *faster* than
/// no limits, which is not a thing. This is the guest that makes the question
/// answerable.
const BUSY: &str = r#"
(component
  (core module $m
    (func (export "spin") (result i32)
      (local $i i32)
      (loop $l
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br_if $l (i32.lt_u (local.get $i) (i32.const 100000))))
      (local.get $i)))
  (core instance $i (instantiate $m))
  (func $spin (result s32) (canon lift (core func $i "spin")))
  (export "spin" (func $spin))
)
"#;

fn host(toml: &str) -> Host {
    Host::builder()
        .manifest(Manifest::parse(toml).expect("manifest"))
        .build()
        .expect("host")
}

fn plugin(host: &Host, wasm: &str) -> Plugin {
    host.load_binary("bench", wasm.as_bytes()).expect("load")
}

fn crossing(c: &mut Criterion) {
    let mut group = c.benchmark_group("crossing");

    // The floor: no limits armed at all.
    let unlimited = host("");
    let mut p = plugin(&unlimited, NOOP);
    group.bench_function("call/no-limits", |b| {
        b.iter(|| black_box(p.call("answer", &[]).expect("call")));
    });

    // Fuel is metered by the engine on every instruction.
    let fuelled = host("[limits]\nfuel = 100_000_000\n");
    let mut p = plugin(&fuelled, NOOP);
    group.bench_function("call/fuel", |b| {
        b.iter(|| black_box(p.call("answer", &[]).expect("call")));
    });

    // A timeout arms epoch interruption and a ticker thread.
    let deadlined = host("[limits]\ntimeout = \"1s\"\n");
    let mut p = plugin(&deadlined, NOOP);
    group.bench_function("call/timeout", |b| {
        b.iter(|| black_box(p.call("answer", &[]).expect("call")));
    });

    // What a host actually configures.
    let both = host("[limits]\nfuel = 100_000_000\ntimeout = \"1s\"\n");
    let mut p = plugin(&both, NOOP);
    group.bench_function("call/fuel+timeout", |b| {
        b.iter(|| black_box(p.call("answer", &[]).expect("call")));
    });

    group.finish();
}

/// What the safety knobs cost when there is something to meter.
///
/// 100k iterations of a counting loop, with and without each limit. The gap is
/// the per-instruction overhead, which is the only form it takes -- none of
/// these cost anything per *crossing*.
/// Is the spread in `crossing/` really per-`Engine` variance?
///
/// `crossing/` reports fuel as faster than no limits, which is not possible, and
/// `docs/PERFORMANCE.md` blamed separate `Engine`s compiling the same guest
/// differently. That was a guess. This is the control that settles it: two hosts
/// built from *identical* manifests, so the only difference between them is that
/// they are different `Engine` instances.
///
/// If these two differ by about as much as `no-limits` and `fuel` do, the
/// spread is the harness and not the limits. If they agree closely, the guess
/// was wrong and something real is going on in `crossing/`.
fn control(c: &mut Criterion) {
    let mut group = c.benchmark_group("control");

    let first = host("");
    let mut p = plugin(&first, NOOP);
    group.bench_function("identical-config/a", |b| {
        b.iter(|| black_box(p.call("answer", &[]).expect("call")));
    });

    let second = host("");
    let mut p = plugin(&second, NOOP);
    group.bench_function("identical-config/b", |b| {
        b.iter(|| black_box(p.call("answer", &[]).expect("call")));
    });

    group.finish();
}

fn metering(c: &mut Criterion) {
    let mut group = c.benchmark_group("metering");

    let unlimited = host("");
    let mut p = plugin(&unlimited, BUSY);
    group.bench_function("spin/no-limits", |b| {
        b.iter(|| black_box(p.call("spin", &[]).expect("call")));
    });

    let fuelled = host("[limits]\nfuel = 100_000_000\n");
    let mut p = plugin(&fuelled, BUSY);
    group.bench_function("spin/fuel", |b| {
        b.iter(|| black_box(p.call("spin", &[]).expect("call")));
    });

    let deadlined = host("[limits]\ntimeout = \"10s\"\n");
    let mut p = plugin(&deadlined, BUSY);
    group.bench_function("spin/timeout", |b| {
        b.iter(|| black_box(p.call("spin", &[]).expect("call")));
    });

    group.finish();
}

fn marshalling(c: &mut Criterion) {
    let mut group = c.benchmark_group("marshalling");
    let host = host("");

    // A string in and a string out: the canonical ABI doing real work.
    let mut p = plugin(&host, ECHO);
    let arg = Val::String("notes.md".into());
    group.bench_function("call/string", |b| {
        b.iter(|| black_box(p.call("echo", std::slice::from_ref(&arg)).expect("call")));
    });

    // The same crossing through WAVE text. The difference is the parse and the
    // render, which the profiler could not see until `wave_nanos` existed --
    // this is that cost, isolated.
    let mut p = plugin(&host, ECHO);
    group.bench_function("call_wave/string", |b| {
        b.iter(|| black_box(p.call_wave("echo", &[r#""notes.md""#]).expect("call")));
    });

    group.finish();
}

fn loading(c: &mut Criterion) {
    let mut group = c.benchmark_group("loading");
    group.sample_size(20); // compilation is slow; the default 100 is wasteful

    // First load of a component this host has never seen: compile included.
    group.bench_function("load/cold", |b| {
        b.iter(|| {
            let host = host("");
            black_box(host.load_binary("bench", NOOP.as_bytes()).expect("load"));
        });
    });

    // Same bytes again on the same host: the compiled-component cache hits, so
    // this is instantiation alone. The gap between the two is what that cache
    // is worth, and why a many-instance host wants it.
    let warm = host("");
    let _first = plugin(&warm, NOOP);
    group.bench_function("load/warm", |b| {
        b.iter(|| black_box(warm.load_binary("again", NOOP.as_bytes()).expect("load")));
    });

    group.finish();
}

criterion_group!(benches, crossing, control, metering, marshalling, loading);
criterion_main!(benches);
