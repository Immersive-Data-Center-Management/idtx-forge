//! Session-owned behavior attach logic.
//!
//! The session owns a *single* [`BehaviorHost`] that drives both the authored
//! runtime behaviors and the XR-preview camera/controller sync behaviors (the
//! unification — retiring the standalone `BehaviorDriver` in
//! `idtx-runtime-core`). This module holds the attach logic in a free function
//! so it stays unit-testable against a bare [`World`] + host, without opening a
//! stage or constructing a whole [`StudioSession`](crate::StudioSession).
//!
//! Least-privilege: a behavior is attached only when *both* its module bytes
//! and its capability manifest resolve. Behaviors that reference an embedded
//! module by uuid (`module_asset == None`) are skipped — those are driven by
//! their owning subsystem (e.g. the XR-preview camera/controller behaviors,
//! attached directly in `xr_preview::XrPreviewSession::start`), never here.

use idtx_behavior::abi::BehaviorKind;
use idtx_behavior::{BehaviorHost, BehaviorManifest, entity_to_wire};
use idtx_scene::StagePlayback;
use idtx_scene::behavior::{BehaviorRef, BehaviorScope};
use idtx_scene::bevy_ecs::prelude::*;
use idtx_scene::components::PrimNode;
use idtx_scene::{NetRole, resolve_owning_player_role};

#[derive(Debug)]
struct AuthoredBinding {
    reference: BehaviorRef,
    manifest: BehaviorManifest,
    uuid: String,
    context: Option<String>,
}

#[derive(Debug, Default, Resource)]
struct AuthoredBindings(std::collections::HashMap<Entity, AuthoredBinding>);

#[derive(Default, Resource)]
struct BindingRevision(u64);

pub(crate) fn binding_revision(world: &World) -> u64 {
    world.get_resource::<BindingRevision>().map_or(0, |revision| revision.0)
}

/// The same eligibility rules govern attachment and spawn readiness. A skipped
/// editor/disabled/remote/stage-disallowed behavior must not block its template.
pub(crate) fn runtime_binding_permitted(
    world: &World,
    entity: Entity,
    reference: &BehaviorRef,
    stage_permitted: impl Fn(&str) -> bool,
) -> bool {
    if !reference.enabled || reference.kind != BehaviorKind::Runtime { return false; }
    let node = world.get::<PrimNode>(entity);
    if node.is_some_and(|node| !node.state.active || !node.state.loaded) { return false; }
    match resolve_owning_player_role(world, entity) {
        NetRole::Local | NetRole::Unowned => {}
        NetRole::Server if reference.server_authoritative => {}
        _ => return false,
    }
    reference.stage_scope != BehaviorScope::Stage
        || stage_permitted(node.map_or("", |node| node.path.as_str()))
}

/// The host binds an authored module under its manifest UUID. Hydrated
/// BehaviorRef UUIDs can be module-path placeholders, so readiness must use
/// the reconciled binding rather than the placeholder.
pub(crate) fn authored_binding_uuid(world: &World, entity: Entity) -> Option<&str> {
    world.get_resource::<AuthoredBindings>()?.0.get(&entity).map(|binding| binding.uuid.as_str())
}

#[derive(Default, Resource)]
struct NewBindings(Vec<(Entity, String)>);

pub(crate) fn initialize_new_bindings(host: &mut BehaviorHost, world: &mut World, playing: bool) {
    for (entity, uuid) in world.remove_resource::<NewBindings>().unwrap_or_default().0 {
        host.initialize_stage_binding(entity_to_wire(entity), &uuid, playing);
    }
}

/// Attach every enabled runtime-kind authored behavior in `world` onto `host`.
///
/// `read_module` resolves a behavior's module to `.wasm` bytes; `read_manifest`
/// resolves its capability manifest. Both are passed `(prim_path, module_asset)`:
/// the behavior's USD prim path (empty when the entity has no [`PrimNode`]) and
/// its authored [`BehaviorRef::module_asset`] string. The prim path lets a reader
/// resolve the module/manifest through USD's own asset resolution — anchored to
/// the authoring layer — with the raw `module_asset` string as a filesystem
/// fallback. Behaviors whose module or manifest is missing — or that reference an
/// embedded module by uuid (`module_asset == None`) — are skipped and logged,
/// never fatal. Returns the count successfully attached.
///
/// **Authority gate:** a behavior is additionally skipped
/// when its owning player subtree is not locally driven. The owning role is
/// resolved by walking up `PrimNode.parent` to the nearest
/// [`idtx_scene::PlayerRole`] via [`resolve_owning_player_role`]:
///
/// * [`NetRole::Local`] / [`NetRole::Unowned`] — attach (the single-player
///   default; `Unowned` means no player attribution, i.e. ordinary scene
///   content, so it must keep ticking).
/// * [`NetRole::Remote`] — **skip** (a replicated proxy for another peer never
///   ticks its own behaviors).
/// * [`NetRole::Server`] — attach only when the behavior opts in via
///   [`BehaviorRef::server_authoritative`]; otherwise skip.
///
/// **Run-condition gate (stage behaviors):** `stage_behavior_permitted` gates
/// every [`BehaviorScope::Stage`] behavior — when `false` (its run scope does not
/// permit the current composition context) the behavior is skipped before its
/// module is read. Entity-scoped behaviors always pass. The caller resolves the
/// permit once from the stage behavior's run scope (read off its *owning* layer)
/// vs the composition context.
///
/// **Binding:** a [`BehaviorScope::Stage`] behavior attaches via
/// [`BehaviorHost::attach_stage`], keyed by its own `/StageBehavior` prim
/// entity (so two composed levels sharing a module uuid stay distinct) yet
/// running stage-wide — [`BehaviorHost::dispatch_stage_event`] reaches it and
/// its `self_entity` is `NULL`; an entity-scoped behavior binds to its own
/// prim's entity.
pub(crate) fn attach_authored_behaviors(
    host: &mut BehaviorHost,
    world: &mut World,
    is_stage_behavior_permitted: impl Fn(&str) -> bool,
    read_module: impl Fn(&str, &str) -> Option<Vec<u8>>,
    read_manifest: impl Fn(&str, &str) -> Option<BehaviorManifest>,
) -> usize {
    attach_authored_behaviors_scoped(host, world, None, is_stage_behavior_permitted, read_module, read_manifest)
}

pub(crate) fn attach_authored_behaviors_scoped(
    host: &mut BehaviorHost,
    world: &mut World,
    scope: Option<&std::collections::HashSet<Entity>>,
    is_stage_behavior_permitted: impl Fn(&str) -> bool,
    read_module: impl Fn(&str, &str) -> Option<Vec<u8>>,
    read_manifest: impl Fn(&str, &str) -> Option<BehaviorManifest>,
) -> usize {
    let revision = binding_revision(world).wrapping_add(1);
    world.insert_resource(BindingRevision(revision));
    let mut bindings = world.remove_resource::<AuthoredBindings>().unwrap_or_default();
    let mut new_bindings = NewBindings::default();
    // Collect targets first so the world borrow is released before attach,
    // which needs `&mut World`. Each target carries the behavior's module asset
    // string, its USD prim path (empty when the entity has no `PrimNode`), and
    // its binding scope (which picks the host wire-entity below).
    let mut targets: Vec<(Entity, String, String, BehaviorRef)> = Vec::new();
    let entities: Vec<_> = if let Some(scope) = scope { scope.iter().copied().collect() }
        else { world.query_filtered::<Entity, With<BehaviorRef>>().iter(world).collect() };
    for entity in entities {
        let Some(bref) = world.get::<BehaviorRef>(entity) else { continue; };
        let node = world.get::<PrimNode>(entity);
        if !runtime_binding_permitted(world, entity, bref, &is_stage_behavior_permitted) {
            continue;
        }
        let Some(path) = &bref.module_asset else {
            continue;
        };
        // Run-condition gate: a stage-scoped behavior runs only when **its own**
        // owning-layer run scope permits the current composition context. Resolved
        // per prim path so, in an assembly surfacing several levels' stage
        // behaviors (each owned by a different layer with its own run scope), one
        // level's gate does not admit or reject another level's behavior.
        let prim_path = node.map(|n| n.path.clone()).unwrap_or_default();
        targets.push((entity, path.clone(), prim_path, bref.clone()));
    }

    let wanted: std::collections::HashSet<_> = targets.iter().map(|(entity, ..)| *entity).collect();
    bindings.0.retain(|entity, binding| {
        if scope.is_some_and(|scope| !scope.contains(entity)) && world.entities().contains(*entity) { return true; }
        if wanted.contains(entity) { return true; }
        let removed = !world.entities().contains(*entity)
            || world.get::<PrimNode>(*entity).is_some_and(|node| !node.state.active || !node.state.loaded);
        if removed { host.destroy(world, entity_to_wire(*entity), &binding.uuid); }
        else { host.detach(world, entity_to_wire(*entity), &binding.uuid); }
        host.release_unused_module(&binding.uuid);
        false
    });

    let mut attached = 0;
    for (entity, path, prim_path, reference) in targets {
        if let Some(error) = world.get::<crate::behavior_bindings::ContextError>(entity) {
            tracing::warn!(path = %prim_path, error = %error.0, "behavior context prevents attachment");
            if let Some(old) = bindings.0.remove(&entity) {
                host.detach(world, entity_to_wire(entity), &old.uuid);
                host.release_unused_module(&old.uuid);
            }
            continue;
        }
        let context = world.get::<idtx_behavior::PrimBehaviorData>(entity).and_then(|data| data.context.clone());
        let Some(manifest) = read_manifest(&prim_path, &path) else {
            if let Some(old) = bindings.0.remove(&entity) {
                host.detach(world, entity_to_wire(entity), &old.uuid);
                host.release_unused_module(&old.uuid);
            }
            continue;
        };
        if bindings.0.get(&entity).is_some_and(|old| old.reference == reference && old.manifest == manifest && old.context == context
            && host.has_instance(entity_to_wire(entity), &old.uuid)) { continue; }
        if let Some(old) = bindings.0.remove(&entity) {
            host.detach(world, entity_to_wire(entity), &old.uuid);
            host.release_unused_module(&old.uuid);
        }
        let Some(bytes) = read_module(&prim_path, &path) else {
            tracing::warn!("skipping runtime behavior: module or manifest missing for {path}");
            continue;
        };
        let scope = reference.stage_scope;
        match host.register_module(&bytes, manifest.clone()) {
            // Stage-scoped behaviors attach via `attach_stage`, keyed by their
            // own `/StageBehavior` prim entity (so two levels sharing a module
            // uuid don't collapse onto one instance) but running stage-wide
            // (`self_entity == NULL`); entity-scoped behaviors bind to their own
            // prim's entity.
            Ok(uuid) => {
                let result = match scope {
                    BehaviorScope::Stage => {
                        host.attach_stage_with_context(world, entity_to_wire(entity), &uuid, context.as_deref())
                    }
                    BehaviorScope::Entity => {
                        host.attach_with_context(world, entity_to_wire(entity), &uuid, context.as_deref())
                    },
                };
                match result {
                    Ok(()) => {
                        attached += 1;
                        new_bindings.0.push((entity, uuid.clone()));
                        bindings.0.insert(entity, AuthoredBinding { reference, manifest, uuid: uuid.clone(), context });
                        tracing::debug!(
                            target: "behavior",
                            %uuid,
                            scope = ?scope,
                            stage_scoped = matches!(scope, BehaviorScope::Stage),
                            "attached authored behavior"
                        );
                    }
                    Err(e) => tracing::warn!("failed to attach behavior {uuid}: {e}"),
                }
            }
            Err(e) => tracing::warn!("failed to register behavior {path}: {e}"),
        }
    }
    world.insert_resource(bindings);
    world.insert_resource(new_bindings);
    attached
}

/// The current stage animation time code carried on `root`'s [`StagePlayback`]
/// clock, or `0.0` when `root` is `None` or has no clock component yet
/// (an unanimated stage). This is the value fed into the behavior host each
/// frame via [`BehaviorHost::set_time_code`], so a guest's `idtx_time_code`
/// import tracks the same clock that drives geometry/skeleton animation.
///
/// Kept a free function so it stays unit-testable against a bare [`World`] with
/// a spawned [`StagePlayback`], without opening a stage.
pub(crate) fn stage_time_code(world: &World, root: Option<Entity>) -> f64 {
    root.and_then(|e| world.get::<StagePlayback>(e))
        .map_or(0.0, |clock| clock.current_time_code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use idtx_behavior::{InputRouter, Limits, PlayState};
    use idtx_scene::components::PrimNode;
    use idtx_scene::{PlayerRole, PrimId, PrimState};
    use idtx_usd::StageGeneration;
    use std::cell::Cell;

    #[test]
    fn authored_attachment_is_idempotent_and_reconciles_enablement() {
        let mut world = World::new();
        let entity = world.spawn(BehaviorRef::new("test").with_module_asset("test.wasm")).id();
        let bytes = wat::parse_str("(module (memory (export \"memory\") 1) (func (export \"_idtx_abi_version\") (result i32) i32.const 3))").unwrap();
        let manifest = BehaviorManifest { name: "test".into(), version: "1".into(), uuid: "test".into(),
            kind: BehaviorKind::Runtime, ship: true, params: Default::default(), inputs: Vec::new(),
            capabilities: idtx_behavior::Capabilities::all(), memory_mb: None };
        let mut host = BehaviorHost::new(Limits::runtime(), InputRouter::default());
        assert_eq!(attach_authored_behaviors(&mut host, &mut world, |_| true, |_, _| Some(bytes.clone()), |_, _| Some(manifest.clone())), 1);
        assert_eq!(attach_authored_behaviors(&mut host, &mut world, |_| true, |_, _| Some(bytes.clone()), |_, _| Some(manifest.clone())), 0);
        world.get_mut::<BehaviorRef>(entity).unwrap().enabled = false;
        attach_authored_behaviors(&mut host, &mut world, |_| true, |_, _| Some(bytes.clone()), |_, _| Some(manifest.clone()));
        assert_eq!(host.instance_count(), 0);
        assert!(!host.has_module("test"));
        world.get_mut::<BehaviorRef>(entity).unwrap().enabled = true;
        world.get_mut::<BehaviorRef>(entity).unwrap().module_asset = Some("other.wasm".into());
        let mut replacement = manifest;
        replacement.uuid = "other".into();
        assert_eq!(attach_authored_behaviors(&mut host, &mut world, |_| true, |_, _| Some(bytes.clone()), |_, _| Some(replacement.clone())), 1);
        assert!(host.has_instance(entity_to_wire(entity), "other"));
        world.despawn(entity);
        attach_authored_behaviors(&mut host, &mut world, |_| true, |_, _| None, |_, _| None);
        assert_eq!(host.instance_count(), 0);
    }

    /// An embedded-by-uuid behavior (`module_asset == None`) is skipped: it is
    /// driven by its owning subsystem (e.g. the XR preview host), not here.
    #[test]
    fn skips_embedded_uuid_behaviors() {
        let mut world = World::new();
        world.spawn(BehaviorRef::new("idtx-embedded-0000"));
        let mut host = BehaviorHost::new(Limits::runtime(), InputRouter::default());
        let attached = attach_authored_behaviors(&mut host, &mut world, |_| true, |_, _| None, |_, _| None);
        assert_eq!(attached, 0);
    }

    /// An authored behavior whose module/manifest cannot be resolved is skipped
    /// (non-fatal), never attached.
    #[test]
    fn skips_authored_behavior_with_unresolvable_module() {
        let mut world = World::new();
        world.spawn(BehaviorRef::new("idtx-authored-0001").with_module_asset("missing.wasm"));
        let mut host = BehaviorHost::new(Limits::runtime(), InputRouter::default());
        let attached = attach_authored_behaviors(&mut host, &mut world, |_| true, |_, _| None, |_, _| None);
        assert_eq!(attached, 0);
        // Ticking with nothing attached is a no-op.
        host.run_frame(&mut world, 1.0 / 60.0, PlayState::Authoring);
    }

    /// The time-code helper reads the pseudo-root's [`StagePlayback`] clock and
    /// defaults to `0.0` when there is no root or no clock (unanimated stage).
    #[test]
    fn stage_time_code_reads_clock_or_defaults_zero() {
        let mut world = World::new();

        // No root entity → default 0.0.
        assert!(stage_time_code(&world, None).abs() < 1e-9);

        // Root without a clock component → default 0.0.
        let bare = world.spawn_empty().id();
        assert!(stage_time_code(&world, Some(bare)).abs() < 1e-9);

        // Root carrying a clock → its current time code.
        let root = world
            .spawn(StagePlayback {
                current_time_code: 42.5,
                ..Default::default()
            })
            .id();
        assert!((stage_time_code(&world, Some(root)) - 42.5).abs() < 1e-9);
    }

    /// Spawn a `PrimNode`-carrying entity so the authority gate's ancestor walk
    /// has parent links to follow.
    fn spawn_prim(world: &mut World, slot: u32, path: &str, parent: Option<PrimId>) -> Entity {
        let prim = PrimId::from_raw_parts(slot, StageGeneration::from_raw(1));
        world
            .spawn(PrimNode {
                prim,
                path: path.to_owned(),
                parent,
                name: path.rsplit('/').next().unwrap_or_default().to_owned(),
                type_name: "Xform".to_owned(),
                state: PrimState {
                    active: true,
                    defined: true,
                    loaded: true,
                },
                is_pseudo_root: false,
                local_layer_identifier: None,
            })
            .id()
    }

    fn prim_id_of(world: &World, entity: Entity) -> PrimId {
        world.get::<PrimNode>(entity).unwrap().prim
    }

    /// Authority gate: a behavior under a `remote`-role pawn is skipped *before*
    /// its module is ever resolved (a replicated proxy never ticks its own
    /// behaviors). We prove the short-circuit by asserting `read_manifest` is
    /// never even called for that entity.
    #[test]
    fn remote_owned_behavior_is_gated_out_before_module_read() {
        let mut world = World::new();
        let player = spawn_prim(&mut world, 1, "/World/Player", None);
        world
            .entity_mut(player)
            .insert(PlayerRole::new(NetRole::Remote));
        let player_prim = prim_id_of(&world, player);

        // Behavior authored on a child of the remote pawn.
        let child = spawn_prim(&mut world, 2, "/World/Player/Gun", Some(player_prim));
        world
            .entity_mut(child)
            .insert(BehaviorRef::new("idtx-authored-0002").with_module_asset("gun.wasm"));

        let reads = Cell::new(0usize);
        let mut host = BehaviorHost::new(Limits::runtime(), InputRouter::default());
        let attached = attach_authored_behaviors(
            &mut host,
            &mut world,
            |_| true,
            |_, _| None,
            |_, _| {
                reads.set(reads.get() + 1);
                None
            },
        );
        assert_eq!(attached, 0);
        assert_eq!(
            reads.get(),
            0,
            "remote pawn must be gated before module read"
        );
    }

    /// Authority gate: a behavior under a `local`-role pawn passes the gate and
    /// reaches manifest resolution (which then fails here, so nothing attaches —
    /// but the manifest lookup proves the gate let it through).
    #[test]
    fn local_owned_behavior_passes_gate() {
        let mut world = World::new();
        let player = spawn_prim(&mut world, 1, "/World/Player", None);
        world
            .entity_mut(player)
            .insert(PlayerRole::new(NetRole::Local));
        let player_prim = prim_id_of(&world, player);

        let child = spawn_prim(&mut world, 2, "/World/Player/Gun", Some(player_prim));
        world
            .entity_mut(child)
            .insert(BehaviorRef::new("idtx-authored-0003").with_module_asset("gun.wasm"));

        let reads = Cell::new(0usize);
        let mut host = BehaviorHost::new(Limits::runtime(), InputRouter::default());
        let _ = attach_authored_behaviors(
            &mut host,
            &mut world,
            |_| true,
            |_, _| None,
            |_, _| {
                reads.set(reads.get() + 1);
                None
            },
        );
        assert_eq!(reads.get(), 1, "local pawn behavior must pass the gate");
    }

    /// Authority gate on a `server`-role pawn: only a behavior that opts in via
    /// `server_authoritative` passes; a plain one is skipped.
    #[test]
    fn server_role_gates_on_server_authoritative_opt_in() {
        let mut world = World::new();
        let player = spawn_prim(&mut world, 1, "/World/Player", None);
        world
            .entity_mut(player)
            .insert(PlayerRole::new(NetRole::Server));
        let player_prim = prim_id_of(&world, player);

        let plain = spawn_prim(&mut world, 2, "/World/Player/Plain", Some(player_prim));
        world
            .entity_mut(plain)
            .insert(BehaviorRef::new("idtx-authored-0004").with_module_asset("plain.wasm"));

        let opted = spawn_prim(&mut world, 3, "/World/Player/Opted", Some(player_prim));
        world.entity_mut(opted).insert(
            BehaviorRef::new("idtx-authored-0005")
                .with_module_asset("opted.wasm")
                .with_server_authoritative(true),
        );

        let read_paths = std::cell::RefCell::new(Vec::<String>::new());
        let mut host = BehaviorHost::new(Limits::runtime(), InputRouter::default());
        let _ = attach_authored_behaviors(
            &mut host,
            &mut world,
            |_| true,
            |_, _| None,
            |_, module_asset| {
                read_paths.borrow_mut().push(module_asset.to_owned());
                None
            },
        );
        let paths = read_paths.into_inner();
        assert!(
            paths.contains(&"opted.wasm".to_owned()),
            "server-authoritative behavior must pass the gate"
        );
        assert!(
            !paths.contains(&"plain.wasm".to_owned()),
            "plain behavior on a server pawn must be gated out"
        );
    }

    /// Run-condition gate: a **stage-scoped** behavior is skipped before its
    /// module is read when `stage_behavior_permitted` is `false` (its run scope
    /// does not permit the current composition context); it passes when `true`.
    #[test]
    fn run_condition_gate_skips_disallowed_stage_behavior() {
        let mut world = World::new();
        let sb = spawn_prim(&mut world, 1, "/StageBehavior", None);
        world.entity_mut(sb).insert(
            BehaviorRef::new("idtx-stage-0001")
                .with_module_asset("stage.wasm")
                .with_scope(BehaviorScope::Stage),
        );

        let reads = Cell::new(0usize);
        let mut host = BehaviorHost::new(Limits::runtime(), InputRouter::default());

        // Not permitted → the stage behavior is gated out before module read.
        let _ = attach_authored_behaviors(
            &mut host,
            &mut world,
            |_| false,
            |_, _| None,
            |_, _| {
                reads.set(reads.get() + 1);
                None
            },
        );
        assert_eq!(reads.get(), 0, "disallowed stage behavior must be gated out");

        // Permitted → reaches module resolution.
        let _ = attach_authored_behaviors(
            &mut host,
            &mut world,
            |_| true,
            |_, _| None,
            |_, _| {
                reads.set(reads.get() + 1);
                None
            },
        );
        assert_eq!(reads.get(), 1, "allowed stage behavior must pass the gate");
    }

    /// An **entity-scoped** behavior is never subject to the stage-behavior
    /// permit — it attaches even when `stage_behavior_permitted` is `false`.
    #[test]
    fn entity_scoped_behavior_ignores_stage_permit() {
        let mut world = World::new();
        let e = spawn_prim(&mut world, 1, "/World/Thing", None);
        world.entity_mut(e).insert(
            BehaviorRef::new("idtx-entity-0001")
                .with_module_asset("thing.wasm")
                .with_scope(BehaviorScope::Entity),
        );

        let reads = Cell::new(0usize);
        let mut host = BehaviorHost::new(Limits::runtime(), InputRouter::default());
        let _ = attach_authored_behaviors(
            &mut host,
            &mut world,
            |_| false,
            |_, _| None,
            |_, _| {
                reads.set(reads.get() + 1);
                None
            },
        );
        assert_eq!(
            reads.get(),
            1,
            "entity-scoped behavior must pass regardless of the stage permit"
        );
    }
}
