//! [`CompiledModule`] — a compiled + validated behavior module (§5).
//!
//! Wraps a [`wasmtime::Module`] plus its parsed manifest and detected
//! lifecycle exports. Validates the guest's `_idtx_abi_version` against
//! the host [`ABI_VERSION`] at instantiation time.

use idtx_behavior_abi::ABI_VERSION;
use wasmtime::{Engine, Instance, Module, Store, ValType};

use crate::limits::Limits;
use crate::linker::build_linker;
use crate::manifest::BehaviorManifest;
use crate::stage_access::HostState;

/// Error surfaced while compiling / validating a module.
#[derive(Debug, thiserror::Error)]
pub enum ModuleError {
    /// wasmtime failed to compile or instantiate.
    #[error("wasmtime error: {0}")]
    Wasm(#[from] wasmtime::Error),
    /// The guest reported an ABI version the host doesn't speak.
    #[error("ABI version mismatch: guest={guest}, host={host}")]
    AbiMismatch { guest: u32, host: u32 },
    /// The module does not export the required ABI-version function.
    #[error("missing required export `_idtx_abi_version` (expected () -> i32)")]
    MissingAbiExport,
    /// The ABI export is not a function with the required signature.
    #[error("invalid `_idtx_abi_version` export: expected a () -> i32 function")]
    InvalidAbiExport,
    /// The host cannot admit additional work/instances under its configured cap.
    #[error("behavior host limit exceeded: {0}")]
    HostLimit(&'static str),
    /// The module referenced an import the granted capabilities don't
    /// provide (capability-denied link failure).
    #[error("module failed to link (likely an ungranted capability import): {0}")]
    LinkFailed(String),
}

/// The lifecycle hooks a module exports (only those actually present).
#[derive(Debug, Clone, Default)]
#[allow(clippy::struct_excessive_bools)] // one flag per lifecycle export
pub struct ExportedHooks {
    pub on_attach: bool,
    pub on_start: bool,
    pub on_update: bool,
    pub on_late_update: bool,
    pub on_editor_update: bool,
    pub on_interact: bool,
    pub on_event: bool,
    pub on_stop: bool,
    pub on_detach: bool,
    pub on_destroy: bool,
    /// Optional stage-level lifecycle broadcast hook (`_idtx_on_stage_event`).
    pub on_stage_event: bool,
    /// Optional input edge hook (`_idtx_on_input`).
    pub on_input: bool,
}

impl ExportedHooks {
    /// Detect which lifecycle exports a compiled module carries.
    fn detect(module: &Module) -> Self {
        let mut h = Self::default();
        for exp in module.exports() {
            match exp.name() {
                "_idtx_on_attach" => h.on_attach = true,
                "_idtx_on_start" => h.on_start = true,
                "_idtx_on_update" => h.on_update = true,
                "_idtx_on_late_update" => h.on_late_update = true,
                "_idtx_on_editor_update" => h.on_editor_update = true,
                "_idtx_on_interact" => h.on_interact = true,
                "_idtx_on_event" => h.on_event = true,
                "_idtx_on_stop" => h.on_stop = true,
                "_idtx_on_detach" => h.on_detach = true,
                "_idtx_on_destroy" => h.on_destroy = true,
                "_idtx_on_stage_event" => h.on_stage_event = true,
                "_idtx_on_input" => h.on_input = true,
                _ => {}
            }
        }
        h
    }
}

/// A compiled, validated behavior module ready to instantiate.
pub struct CompiledModule {
    /// The parsed manifest (capabilities, kind, params, inputs).
    pub manifest: BehaviorManifest,
    /// The compiled wasmtime module.
    pub module: Module,
    /// Detected lifecycle exports.
    pub hooks: ExportedHooks,
}

impl std::fmt::Debug for CompiledModule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompiledModule")
            .field("uuid", &self.manifest.uuid)
            .field("kind", &self.manifest.kind)
            .field("hooks", &self.hooks)
            .finish_non_exhaustive()
    }
}

impl CompiledModule {
    /// Compile `wasm_bytes` against `engine` and validate it against
    /// `manifest`.
    pub fn compile(
        engine: &Engine,
        wasm_bytes: &[u8],
        manifest: BehaviorManifest,
    ) -> Result<Self, ModuleError> {
        let module = Module::new(engine, wasm_bytes)?;
        let hooks = ExportedHooks::detect(&module);
        Ok(Self {
            manifest,
            module,
            hooks,
        })
    }

    /// Instantiate this module into a fresh [`Store`], validating the
    /// ABI-version export.
    ///
    /// The `state` seeds capabilities, self-entity, limits, etc. Returns
    /// the store + instance so the caller can drive lifecycle hooks.
    /// The ABI export's signature is checked before executing guest code. One
    /// startup fuel/epoch budget covers initialization and the ABI call; normal
    /// lifecycle hooks receive their own budgets from the dispatcher.
    ///
    /// `engine` must enable fuel metering and epoch interruption, with a live
    /// epoch ticker (as provided by [`crate::BehaviorHost`]). Compilation and
    /// import linking are outside the guest startup budget.
    pub fn instantiate(
        &self,
        engine: &Engine,
        state: HostState,
        limits: Limits,
    ) -> Result<(Store<HostState>, Instance), ModuleError> {
        let abi_export = self
            .module
            .get_export("_idtx_abi_version")
            .ok_or(ModuleError::MissingAbiExport)?;
        let abi_type = abi_export.func().ok_or(ModuleError::InvalidAbiExport)?;
        if abi_type.params().len() != 0
            || abi_type.results().len() != 1
            || !matches!(abi_type.results().next(), Some(ValType::I32))
        {
            return Err(ModuleError::InvalidAbiExport);
        }

        let linker = build_linker(engine, self.manifest.capabilities)?;
        // Resolve imports separately so a start-function trap retains its
        // wasmtime cause rather than being mislabeled as a capability/link error.
        let pre = linker
            .instantiate_pre(&self.module)
            .map_err(|e| ModuleError::LinkFailed(e.to_string()))?;
        let mut store = Store::new(engine, state);
        // Wire the resource limiter (linear-memory cap).
        store.limiter(|s| &mut s.limits);
        // Both calls execute untrusted guest code. Arm once: the ABI probe gets
        // only the fuel and time left after module initialization. Propagate a
        // fuel-configuration error rather than silently running without a guard.
        store.set_fuel(limits.startup_fuel)?;
        store.set_epoch_deadline(limits.startup_epoch_deadline_ticks());
        let instance = pre.instantiate(&mut store)?;

        let abi_fn = instance.get_typed_func::<(), u32>(&mut store, "_idtx_abi_version")?;
        let guest = abi_fn.call(&mut store, ())?;
        if guest != ABI_VERSION {
            return Err(ModuleError::AbiMismatch {
                guest,
                host: ABI_VERSION,
            });
        }

        Ok((store, instance))
    }
}
