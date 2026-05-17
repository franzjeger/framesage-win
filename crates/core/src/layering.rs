//! Item 3.8 — automated enforcement of the workspace layering rules
//! documented in `ARCHITECTURE.md`.
//!
//! This module ships one test (`workspace_layering_invariants_hold`)
//! that runs `cargo metadata --no-deps` and asserts the framesage-* →
//! framesage-* dependency edges match the allowlist below. Adding an
//! edge that isn't in the list fails the test; removing an allowed
//! edge from the workspace's Cargo.toml does NOT fail (the test only
//! catches additions, not deletions).
//!
//! The test runs at test time, not build time, so the only cost is
//! one `cargo` invocation during `cargo test -p framesage-core`. We
//! intentionally don't enforce this via `build.rs` — that would
//! re-run the check on every recompile and slow down the inner
//! development loop.
//!
//! Lives in `framesage-core` because:
//!
//! * Core has `serde_json` already (used by other tests).
//! * Core is the bottom of the dep graph, so the layering rules are
//!   most relevant here.
//! * Core's test surface is small and any test failure surfaces
//!   immediately on the first `cargo test`.

#![cfg(test)]

use std::process::Command;

use serde::Deserialize;

/// Allowlist of `framesage-* → framesage-*` dependency edges. Mirrors
/// the diagram in ARCHITECTURE.md. Tuples are `(from, [allowed
/// targets])`. Crates not in this list (e.g. `framesage-sim` if
/// nothing depends on it) implicitly have no incoming edges.
const ALLOWED_EDGES: &[(&str, &[&str])] = &[
    // Bottom of the stack — zero framesage deps.
    ("framesage-core", &[]),
    ("framesage-ipc", &["framesage-core"]),
    ("framesage-gamemode", &["framesage-core"]),
    // The one inversion: sys imports gamemode for the Win32 impl of
    // SystemStateQuery + the AppliedActions / PreviousState data
    // shapes. Contained to inner::game_mode.
    ("framesage-sys", &["framesage-core", "framesage-gamemode"]),
    // Sim drives the gamemode planner with synthetic state — no sys,
    // no engine, no ipc.
    ("framesage-sim", &["framesage-core", "framesage-gamemode"]),
    // Engine is the orchestrator.
    (
        "framesage-engine",
        &[
            "framesage-core",
            "framesage-ipc",
            "framesage-gamemode",
            "framesage-sys",
        ],
    ),
    // v0.7 Group A: new bottom-of-stack crate for the closed-loop
    // ETW kernel-event consumer. Zero framesage deps — wraps the
    // Windows ETW API surface and exposes the consumer-lifecycle
    // + degradation types. Only `framesage-service` depends on it
    // (the service host spawns the supervisor task).
    ("framesage-etw", &[]),
    // Only the service depends on engine (and on etw, added v0.7).
    (
        "framesage-service",
        &[
            "framesage-core",
            "framesage-engine",
            "framesage-etw",
            "framesage-gamemode",
            "framesage-ipc",
            "framesage-sys",
        ],
    ),
    // CLI is a thin IPC client (no engine).
    (
        "framesage-cli",
        &[
            "framesage-core",
            "framesage-gamemode",
            "framesage-ipc",
            "framesage-sys",
        ],
    ),
    // Tray is a thin IPC client (no engine). Item 4.13 added a
    // `framesage-gamemode` edge so the tray's profile editor can
    // surface the bundled SafeList (denylist rationale strings)
    // inline as the user types — a non-overridable read of static
    // data, no behavior coupling.
    (
        "framesage-tray",
        &[
            "framesage-core",
            "framesage-gamemode",
            "framesage-ipc",
            "framesage-sys",
        ],
    ),
];

#[derive(Debug, Deserialize)]
struct CargoMetadata {
    packages: Vec<Package>,
}

#[derive(Debug, Deserialize)]
struct Package {
    name: String,
    dependencies: Vec<Dependency>,
}

#[derive(Debug, Deserialize)]
struct Dependency {
    name: String,
}

/// Runs `cargo metadata --no-deps --format-version 1` against the
/// workspace and asserts every framesage-* package's framesage-*
/// dependency list matches the allowlist.
///
/// `--no-deps` keeps the output small (just workspace members, no
/// transitive crates); `--format-version 1` pins the JSON schema so
/// upstream cargo changes can't silently break the parse.
#[test]
fn workspace_layering_invariants_hold() {
    // The test runs from `crates/core/`. Walk two levels up to the
    // workspace root manifest.
    let manifest_path = std::env::current_dir()
        .expect("current_dir")
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.join("Cargo.toml"))
        .expect("workspace Cargo.toml");

    let output = Command::new(env!("CARGO"))
        .args([
            "metadata",
            "--no-deps",
            "--format-version",
            "1",
            "--manifest-path",
        ])
        .arg(&manifest_path)
        .output()
        .expect("invoke cargo metadata");
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let metadata: CargoMetadata =
        serde_json::from_slice(&output.stdout).expect("parse cargo metadata JSON");

    // Build an actual-edges map from the cargo metadata output.
    // We only care about framesage-* → framesage-* edges.
    let mut violations: Vec<String> = Vec::new();
    for pkg in &metadata.packages {
        if !pkg.name.starts_with("framesage-") {
            continue;
        }
        let allowed: &[&str] = match ALLOWED_EDGES.iter().find(|(p, _)| *p == pkg.name) {
            Some((_, list)) => list,
            None => {
                violations.push(format!(
                    "package `{}` is not in ALLOWED_EDGES — add it to crates/core/src/layering.rs",
                    pkg.name
                ));
                continue;
            }
        };
        for dep in &pkg.dependencies {
            if !dep.name.starts_with("framesage-") {
                continue;
            }
            if !allowed.contains(&dep.name.as_str()) {
                violations.push(format!(
                    "layering violation: `{}` depends on `{}` — not in the allowlist for `{}` \
                     (see ARCHITECTURE.md)",
                    pkg.name, dep.name, pkg.name
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "{} layering violation(s):\n{}",
        violations.len(),
        violations.join("\n")
    );
}
