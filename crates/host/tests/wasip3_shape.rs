//! Does the capability model survive WASI 0.3?
//!
//! `CLAUDE.md` has said since M1 that the permission model is kept "0.3-shaped"
//! so that the upgrade is additive. That was a design intention with nothing
//! checking it. This file checks it, against the interface names read out of
//! `wasmtime-wasi` 48.0.1's own `src/p3/wit/deps/*.wit` rather than guessed.
//!
//! It found one hole, which is now fixed and pinned below: `wall-clock` is
//! called `system-clock` in 0.3, and the old fallthrough classified it as
//! `MonotonicClock`. See ADR-0013.
//!
//! watoots does not *serve* p3 — the host links wasip2 only, and
//! `wasmtime-wasi`'s p3 module is feature-gated and documents itself as not
//! ready for production. Classification runs before any of that, so these
//! answers are what `watoots inspect` would print about a p3 component today.

use std::collections::BTreeSet;
use watoots::imports::{ComponentImport, Requirement, classify};

fn requirement(name: &str) -> Requirement {
    classify(
        ComponentImport {
            name,
            has_functions: true,
        },
        &BTreeSet::new(),
    )
}

#[test]
fn every_p3_interface_lands_on_the_same_capability_as_its_0_2_equivalent() {
    let expected = [
        ("wasi:cli/environment@0.3.0", Requirement::Environment),
        ("wasi:cli/exit@0.3.0", Requirement::Ambient),
        ("wasi:cli/stdin@0.3.0", Requirement::Ambient),
        ("wasi:cli/stdout@0.3.0", Requirement::Ambient),
        ("wasi:cli/stderr@0.3.0", Requirement::Ambient),
        (
            "wasi:clocks/monotonic-clock@0.3.0",
            Requirement::MonotonicClock,
        ),
        ("wasi:filesystem/types@0.3.0", Requirement::Filesystem),
        ("wasi:filesystem/preopens@0.3.0", Requirement::Filesystem),
        ("wasi:random/random@0.3.0", Requirement::Random),
        ("wasi:random/insecure@0.3.0", Requirement::Random),
        ("wasi:random/insecure-seed@0.3.0", Requirement::Random),
        ("wasi:sockets/types@0.3.0", Requirement::Network),
        ("wasi:sockets/ip-name-lookup@0.3.0", Requirement::Network),
    ];

    for (name, want) in expected {
        assert_eq!(requirement(name), want, "{name}");
    }
}

#[test]
fn the_0_3_wall_clock_is_a_wall_clock_under_its_new_name() {
    // The hole. `wasi:clocks/wall-clock` became `wasi:clocks/system-clock`, and
    // a `("clocks", _)` fallthrough sent it to `MonotonicClock` — so a manifest
    // that said `clocks = "monotonic"` in order to keep real time away from a
    // plugin would have granted exactly that. Both spellings, one capability.
    assert_eq!(
        requirement("wasi:clocks/wall-clock@0.2.12"),
        Requirement::WallClock
    );
    assert_eq!(
        requirement("wasi:clocks/system-clock@0.3.0"),
        Requirement::WallClock
    );
    assert_ne!(
        requirement("wasi:clocks/system-clock@0.3.0"),
        Requirement::MonotonicClock,
        "the real-time clock must never ride in on a monotonic grant"
    );
}

#[test]
fn timezone_is_not_monotonic() {
    // Added in 0.3. It converts an instant to a human calendar offset and says
    // where the host thinks it is, so it belongs with the wall clock.
    assert_eq!(
        requirement("wasi:clocks/timezone@0.3.0"),
        Requirement::WallClock
    );
}

#[test]
fn an_unread_clocks_interface_denies_rather_than_picking_a_side() {
    // The property that would have prevented the bug above: a `clocks`
    // interface nobody has classified is refused, not assigned to whichever
    // capability a fallthrough happened to name.
    assert_eq!(
        requirement("wasi:clocks/some-future-clock@0.4.0"),
        Requirement::Unrecognized
    );
}

#[test]
fn a_p3_only_interface_we_have_not_read_denies() {
    // `wasi:cli/types` is new in 0.3. A real component would import it for its
    // types alone and classify as `TypesOnly`; forced to look callable, it
    // denies. Both answers are safe, and neither is a silent grant.
    assert_eq!(
        requirement("wasi:cli/types@0.3.0"),
        Requirement::Unrecognized
    );
    assert_eq!(
        classify(
            ComponentImport {
                name: "wasi:cli/types@0.3.0",
                has_functions: false,
            },
            &BTreeSet::new(),
        ),
        Requirement::TypesOnly
    );
}
