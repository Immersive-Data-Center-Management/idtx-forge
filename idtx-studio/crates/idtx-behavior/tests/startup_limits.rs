//! Startup regressions against the real host, including native and Pulley engines.
//!
//! Run with `cargo test -p idtx-behavior --test startup_limits` and again with
//! `--features visionos`. Runaway fixtures execute in killable child processes so
//! a broken execution guard fails the test instead of hanging the test runner.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};
mod common;
use common::with_watchdog;

use bevy_ecs::world::World;
use idtx_behavior::{
    ABI_VERSION, BehaviorHost, BehaviorManifest, Capabilities, InputRouter, Limits, ModuleError,
    entity_to_wire,
};
use idtx_behavior_abi::BehaviorKind;

const INFINITE_START: &str = r#"
(module
  (func $start (loop br 0))
  (start $start)
  (func (export "_idtx_abi_version") (result i32) i32.const 3))
"#;

const INFINITE_ABI: &str = r#"
(module
  (func (export "_idtx_abi_version") (result i32)
    (loop br 0)
    i32.const 3))
"#;

fn registered_host(wat: &str, limits: Limits) -> BehaviorHost {
    let wasm = wat::parse_str(wat).expect("valid test WAT");
    let manifest = BehaviorManifest {
        name: "startup fixture".to_owned(),
        version: "0.1.0".to_owned(),
        uuid: "startup-fixture".to_owned(),
        kind: BehaviorKind::Runtime,
        ship: true,
        params: BTreeMap::new(),
        inputs: Vec::new(),
        capabilities: Capabilities::default(),
        memory_mb: None,
    };
    let mut host = BehaviorHost::new(limits, InputRouter::default());
    assert_eq!(host.engine().is_pulley(), cfg!(feature = "visionos"));
    host.register_module(&wasm, manifest).expect("module compiles");
    host
}

fn attach(host: &mut BehaviorHost) -> Result<(), ModuleError> {
    let mut world = World::new();
    let entity = entity_to_wire(world.spawn_empty().id());
    host.attach(&mut world, entity, "startup-fixture")
}

fn assert_bounded_rejection(wat: &str, limits: Limits) -> wasmtime::Trap {
    let mut host = registered_host(wat, limits);
    // Compilation is deliberately outside the guest-execution measurement.
    let start = Instant::now();
    let error = attach(&mut host).expect_err("runaway startup must be rejected");
    let elapsed = start.elapsed();
    assert!(elapsed < Duration::from_secs(1), "startup took {elapsed:?}");
    assert_eq!(host.instance_count(), 0, "failed startup must not attach");
    let ModuleError::Wasm(error) = error else {
        panic!("expected a guest execution failure, got {error:?}");
    };
    match error.downcast_ref::<wasmtime::Trap>() {
        Some(wasmtime::Trap::OutOfFuel) => wasmtime::Trap::OutOfFuel,
        Some(wasmtime::Trap::Interrupt) => wasmtime::Trap::Interrupt,
        _ => panic!("expected a budget trap, got {error:?}"),
    }
}

#[test]
fn infinite_start_is_bounded() {
    with_watchdog("infinite_start_is_bounded", || {
        assert_bounded_rejection(INFINITE_START, Limits::runtime());
    });
}

#[test]
fn infinite_abi_is_bounded() {
    with_watchdog("infinite_abi_is_bounded", || {
        assert_bounded_rejection(INFINITE_ABI, Limits::runtime());
    });
}

#[test]
fn missing_abi_export_is_rejected() {
    let mut host = registered_host("(module)", Limits::runtime());
    let error = attach(&mut host).expect_err("ABI export is mandatory");
    assert!(matches!(error, ModuleError::MissingAbiExport));
    assert!(error.to_string().contains("_idtx_abi_version"), "{error}");
    assert_eq!(host.instance_count(), 0);
}

#[test]
fn mistyped_abi_exports_are_rejected() {
    for export in [
        r#"(func (export "_idtx_abi_version") (result i64) i64.const 3)"#,
        r#"(func (export "_idtx_abi_version") (param i32) (result i32) i32.const 3)"#,
        r#"(func (export "_idtx_abi_version"))"#,
        r#"(func (export "_idtx_abi_version") (result i32 i32) i32.const 3 i32.const 3)"#,
        r#"(global (export "_idtx_abi_version") i32 (i32.const 3))"#,
        r#"(memory (export "_idtx_abi_version") 1)"#,
    ] {
        let mut host = registered_host(&format!("(module {export})"), Limits::runtime());
        let error = attach(&mut host).expect_err("ABI must be a () -> i32 function");
        assert!(matches!(error, ModuleError::InvalidAbiExport));
        assert!(error.to_string().contains("_idtx_abi_version"), "{error}");
        assert_eq!(host.instance_count(), 0);
    }
}

#[test]
fn abi_export_is_validated_before_start_executes() {
    for export in [
        "",
        r#"(func (export "_idtx_abi_version") (result i64) i64.const 3)"#,
    ] {
        let wat = format!("(module (func $start unreachable) (start $start) {export})");
        let mut host = registered_host(&wat, Limits::runtime());
        let error = attach(&mut host).expect_err("invalid ABI must reject before the start trap");
        assert!(matches!(error, ModuleError::MissingAbiExport | ModuleError::InvalidAbiExport));
        assert_eq!(host.instance_count(), 0);
    }
}

#[test]
fn incompatible_abi_version_is_rejected() {
    let wat = format!(
        r#"(module (func (export "_idtx_abi_version") (result i32) i32.const {}))"#,
        ABI_VERSION + 1,
    );
    let mut host = registered_host(&wat, Limits::runtime());
    let error = attach(&mut host).expect_err("unsupported ABI must reject");
    assert!(matches!(error, ModuleError::AbiMismatch { guest, host }
        if guest == ABI_VERSION + 1 && host == ABI_VERSION));
    assert_eq!(host.instance_count(), 0);
}

#[test]
fn start_fuel_guard_is_independent_of_epochs() {
    with_watchdog("start_fuel_guard_is_independent_of_epochs", || {
        let limits = Limits {
            startup_fuel: 1_000,
            startup_budget: Duration::from_secs(2),
            ..Limits::runtime()
        };
        assert_eq!(assert_bounded_rejection(INFINITE_START, limits), wasmtime::Trap::OutOfFuel);
    });
}

#[test]
fn abi_fuel_guard_is_independent_of_epochs() {
    with_watchdog("abi_fuel_guard_is_independent_of_epochs", || {
        let limits = Limits {
            startup_fuel: 1_000,
            startup_budget: Duration::from_secs(2),
            ..Limits::runtime()
        };
        assert_eq!(assert_bounded_rejection(INFINITE_ABI, limits), wasmtime::Trap::OutOfFuel);
    });
}

#[test]
fn start_epoch_guard_is_independent_of_fuel() {
    with_watchdog("start_epoch_guard_is_independent_of_fuel", || {
        let limits = Limits {
            startup_fuel: u64::MAX,
            startup_budget: Duration::from_millis(10),
            ..Limits::runtime()
        };
        assert_eq!(assert_bounded_rejection(INFINITE_START, limits), wasmtime::Trap::Interrupt);
    });
}

#[test]
fn abi_epoch_guard_is_independent_of_fuel() {
    with_watchdog("abi_epoch_guard_is_independent_of_fuel", || {
        let limits = Limits {
            startup_fuel: u64::MAX,
            startup_budget: Duration::from_millis(10),
            ..Limits::runtime()
        };
        assert_eq!(assert_bounded_rejection(INFINITE_ABI, limits), wasmtime::Trap::Interrupt);
    });
}

/// Finite work in each startup phase, with globals checking their ordering.
/// Hooks do more work independently to exercise the startup-to-hook handoff.
fn finite_startup(start_work: u32, abi_work: u32, hook_work: u32) -> String {
    format!(r#"
(module
  (global $phase (mut i32) (i32.const 0))
  (func $burn (param $n i32)
    (loop $again
      (if (i32.eqz (local.get $n)) (then return))
      (local.set $n (i32.sub (local.get $n) (i32.const 1)))
      br $again))
  (func $start
    (call $burn (i32.const {start_work}))
    (global.set $phase (i32.const 1)))
  (start $start)
  (func (export "_idtx_abi_version") (result i32)
    (if (i32.ne (global.get $phase) (i32.const 1)) (then unreachable))
    (call $burn (i32.const {abi_work}))
    (global.set $phase (i32.const 2))
    i32.const {ABI_VERSION})
  (func (export "_idtx_on_attach")
    (if (i32.ne (global.get $phase) (i32.const 2)) (then unreachable))
    (call $burn (i32.const {hook_work}))
    (global.set $phase (i32.const 3)))
  (func (export "_idtx_on_update") (param f32)
    (if (i32.ne (global.get $phase) (i32.const 3)) (then unreachable))
    (call $burn (i32.const {hook_work}))))
"#)
}

#[test]
fn start_and_abi_share_one_fuel_budget() {
    let limits = Limits {
        startup_fuel: 10_000,
        startup_budget: Duration::from_secs(1),
        ..Limits::runtime()
    };
    // Either phase fits alone; together they must exceed the one startup cap.
    for (start_work, abi_work) in [(1_000, 0), (0, 1_000)] {
        let mut host = registered_host(&finite_startup(start_work, abi_work, 0), limits);
        attach(&mut host).expect("individual startup phase fits the fuel budget");
        assert_eq!(host.instance_count(), 1);
    }
    assert_eq!(
        assert_bounded_rejection(&finite_startup(1_000, 1_000, 0), limits),
        wasmtime::Trap::OutOfFuel,
    );
}

#[test]
fn valid_startup_leaves_hooks_their_own_budget() {
    let limits = Limits {
        startup_fuel: 1_000,
        startup_budget: Duration::from_secs(1),
        hook_fuel: 50_000,
        hook_budget: Duration::from_secs(1),
        ..Limits::runtime()
    };
    let mut host = registered_host(&finite_startup(10, 10, 1_000), limits);
    let mut world = World::new();
    let entity = entity_to_wire(world.spawn_empty().id());
    host.attach(&mut world, entity, "startup-fixture").expect("valid startup");
    host.run_frame(&mut world, 0.016, idtx_behavior::PlayState::Playing);
    assert_eq!(host.instance_count(), 1);
    assert_eq!(host.diagnostics(entity, "startup-fixture").unwrap().2, 0, "hooks trapped");
}

#[test]
fn zero_startup_fuel_is_not_unlimited() {
    let limits = Limits {
        startup_fuel: 0,
        ..Limits::runtime()
    };
    assert_eq!(
        assert_bounded_rejection(&finite_startup(0, 0, 0), limits),
        wasmtime::Trap::OutOfFuel,
    );
}

#[test]
fn startup_faults_preserve_the_guest_trap() {
    for wat in [
        r#"(module (func $start unreachable) (start $start)
            (func (export "_idtx_abi_version") (result i32) i32.const 3))"#,
        r#"(module (func (export "_idtx_abi_version") (result i32) unreachable))"#,
    ] {
        let mut host = registered_host(wat, Limits::runtime());
        let error = attach(&mut host).expect_err("guest startup fault must reject");
        let ModuleError::Wasm(error) = error else {
            panic!("guest fault was mislabeled: {error:?}");
        };
        assert_eq!(error.downcast_ref::<wasmtime::Trap>(), Some(&wasmtime::Trap::UnreachableCodeReached));
        assert_eq!(host.instance_count(), 0);
    }
}
