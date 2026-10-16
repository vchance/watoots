//! Compiled components, kept so a second instance costs a `Store` and not a
//! compile, and the hashing that keys them.
//!
//! The lookup and the coalescing of concurrent misses are `Host::compile`'s;
//! this is the bounded map behind it and the digests it and the audit trail
//! name bytes by.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::hash::{Hash, Hasher};

use sha2::{Digest, Sha256};
use wasmtime::Engine;
use wasmtime::component::Component;

/// How many distinct components a host keeps compiled code for.
///
/// Sized for the case this cache exists to serve — a host with a handful of
/// plugins, instantiated many times each — rather than for the case that needs
/// the bound. Every realistic plugin set fits, and a host that has genuinely
/// churned through this many distinct components has told us the oldest are
/// not coming back.
const COMPILED_CACHE_CAPACITY: usize = 32;

/// Compiled components, keyed by content hash, bounded and evicted oldest-first.
///
/// Not an LRU: a load is not a use worth reordering the queue for, and reading
/// under a shared lock is worth more than eviction precision. The distinction
/// only matters once a host exceeds the capacity, which is already the unusual
/// case this exists to survive rather than to optimise.
#[derive(Default)]
pub(crate) struct CompiledCache {
    by_key: BTreeMap<String, Component>,
    insertion_order: VecDeque<String>,
}

impl CompiledCache {
    pub(crate) fn get(&self, key: &str) -> Option<&Component> {
        self.by_key.get(key)
    }

    pub(crate) fn insert(&mut self, key: String, component: Component) {
        if self.by_key.insert(key.clone(), component).is_some() {
            // Already present and already queued. `Host::compile` coalesces
            // concurrent misses, so this should not happen; it is kept because
            // queueing a key twice would evict a live entry early and leave a
            // dangling name behind it, and a belt is cheap.
            return;
        }
        self.insertion_order.push_back(key);
        while self.insertion_order.len() > COMPILED_CACHE_CAPACITY {
            if let Some(oldest) = self.insertion_order.pop_front() {
                self.by_key.remove(&oldest);
            }
        }
    }
}

/// Feeds `std::hash::Hash` output into SHA-256.
///
/// Wasmtime hands out its compatibility fingerprint as an opaque `impl Hash`,
/// and `DefaultHasher` is explicitly not stable across releases — no use for
/// something that names files on disk.
#[derive(Default)]
struct Sha256Hasher(Sha256);

impl Hasher for Sha256Hasher {
    fn write(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    fn finish(&self) -> u64 {
        let digest = self.0.clone().finalize();
        u64::from_le_bytes(digest[..8].try_into().expect("sha256 is 32 bytes"))
    }
}

/// SHA-256 of a component, lowercase hex.
///
/// The same digest `watoots_trace::Trace::hash_component` writes into a trace
/// header, so an audit line and a recording name the same bytes the same way.
/// Only ever called when an audit hook is installed.
pub(crate) fn sha256_hex(wasm: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(wasm);
    hex(&hasher.finalize())
}

fn hex(bytes: &[u8]) -> String {
    use fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

/// Cache key: this engine's configuration plus the exact component bytes.
///
/// The engine half is what makes reusing a `.cwasm` safe — a cache written by a
/// differently configured engine simply does not collide.
pub(crate) fn cache_key(engine: &Engine, wasm: &[u8]) -> String {
    let mut hasher = Sha256Hasher::default();
    engine.precompile_compatibility_hash().hash(&mut hasher);
    hasher.0.update(wasm);
    hex(&hasher.0.finalize())
}
