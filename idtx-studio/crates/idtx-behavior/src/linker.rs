//! Capability-gated [`wasmtime::Linker`] construction (§5.3).
//!
//! Only the buckets a manifest grants are linked; an ungranted import is
//! simply absent, so a module referencing it fails to link —
//! compile-time-ish capability enforcement. `log` + `time` +
//! `entity_self` are always linked (they carry no authority).
//!
//! Every import reads/writes guest linear memory through the
//! [`wasmtime::Caller`] and routes into the [`HostState`]'s
//! [`crate::stage_access`] accessors.

use idtx_behavior_abi::{
    ArcCastParams, ArcHitResult, Entity as WireEntity, ErrorCode, Mat4 as WireMat4,
    OverlayCommandKind, PlaybackCommandKind, RayHitResult, RaycastParams, Vec3 as WireVec3, Xform,
    XrHandJointResult, XrPoseResult, XrSide, XrSpace,
};
use wasmtime::{Caller, Engine, Linker};

use crate::manifest::Capabilities;
use crate::overlay::OverlayOp;
use crate::session_edit::SessionEditOp;
use crate::stage_access::HostState;

/// Upper bound on the arc-cast polyline sample buffer the host will allocate for
/// a guest `idtx_arc_cast` call. The guest supplies the requested capacity, so it
/// must be clamped before the host-side allocation (see the import) — an
/// unclamped `u32::MAX` would be a ~48 GiB native allocation outside the wasm
/// sandbox's memory guards. A real ballistic arc is a handful of samples.
const MAX_ARC_SAMPLES: usize = 4096;

/// Build a linker exposing exactly the host imports the `caps` grant.
pub fn build_linker(
    engine: &Engine,
    caps: Capabilities,
) -> Result<Linker<HostState>, wasmtime::Error> {
    let mut linker = Linker::new(engine);

    // -- always linked: log, time, entity_self --------------------
    linker.func_wrap(
        "idtx",
        "idtx_log",
        |mut caller: Caller<'_, HostState>, level: u32, ptr: u32, len: u32| {
            if let Some(msg) = read_string(&mut caller, ptr, len) {
                match level {
                    0 => tracing::error!(target: "behavior", "{msg}"),
                    1 => tracing::warn!(target: "behavior", "{msg}"),
                    2 => tracing::info!(target: "behavior", "{msg}"),
                    _ => tracing::debug!(target: "behavior", "{msg}"),
                }
            }
        },
    )?;

    linker.func_wrap(
        "idtx",
        "idtx_delta_seconds",
        |caller: Caller<'_, HostState>| caller.data().delta_seconds,
    )?;
    linker.func_wrap("idtx", "idtx_time_code", |caller: Caller<'_, HostState>| {
        caller.data().time_code
    })?;
    linker.func_wrap(
        "idtx",
        "idtx_entity_self",
        |caller: Caller<'_, HostState>| caller.data().self_entity.to_bits(),
    )?;
    // Active view/player camera pose (stage space). Always linked — ambient
    // frame state like time; writes a 40-byte `CameraPose` and returns Ok.
    linker.func_wrap(
        "idtx",
        "idtx_camera_pose",
        |mut caller: Caller<'_, HostState>, out_ptr: u32| -> u32 {
            let pose = caller.data().camera_pose();
            write_bytes(&mut caller, out_ptr, bytemuck::bytes_of(&pose));
            ErrorCode::Ok.as_u32()
        },
    )?;
    // Active view/player camera world transform (stage space). Sibling of
    // `idtx_camera_pose` — always linked (ambient); writes a 64-byte `Mat4`
    // (identity when no camera this frame) and returns Ok.
    linker.func_wrap(
        "idtx",
        "idtx_camera_transform",
        |mut caller: Caller<'_, HostState>, out_ptr: u32| -> u32 {
            let xf = WireMat4::from(caller.data().camera_transform());
            write_bytes(&mut caller, out_ptr, bytemuck::bytes_of(&xf));
            ErrorCode::Ok.as_u32()
        },
    )?;

    // Immutable own context is intrinsic to a binding, like entity_self.
    linker.func_wrap(
        "idtx",
        "idtx_behavior_context",
        |mut caller: Caller<'_, HostState>, ptr: u32, cap: u32, len_ptr: u32| -> u32 {
            let Some(context) = caller.data().behavior_context.clone() else {
                return ErrorCode::NotFound.as_u32();
            };
            write_json_result(&mut caller, ptr, cap, len_ptr, &context)
        },
    )?;
    linker.func_wrap(
        "idtx",
        "idtx_hook_result",
        |mut caller: Caller<'_, HostState>, code: u32| {
            if code != 0 {
                caller.data_mut().hook_error = Some(ErrorCode::from_u32(code));
            }
        },
    )?;

    // -- xform (xform_write for set; reads gated by scene_query OR
    //    xform_write so a writer can also read) --------------------
    if caps.xform_write || caps.scene_query {
        linker.func_wrap(
            "idtx",
            "idtx_get_xform",
            |mut caller: Caller<'_, HostState>, entity: u64, out_ptr: u32| -> u32 {
                let wire = WireEntity::from_bits(entity);
                match caller.data().get_xform(wire) {
                    Ok(x) => {
                        write_xform(&mut caller, out_ptr, &x);
                        ErrorCode::Ok.as_u32()
                    }
                    Err(e) => e.as_u32(),
                }
            },
        )?;
        linker.func_wrap(
            "idtx",
            "idtx_world_xform",
            |mut caller: Caller<'_, HostState>, entity: u64, out_ptr: u32| -> u32 {
                let wire = WireEntity::from_bits(entity);
                match caller.data().world_xform(wire) {
                    Ok(x) => {
                        write_xform(&mut caller, out_ptr, &x);
                        ErrorCode::Ok.as_u32()
                    }
                    Err(e) => e.as_u32(),
                }
            },
        )?;
    }
    if caps.xform_write {
        linker.func_wrap(
            "idtx",
            "idtx_set_xform",
            |mut caller: Caller<'_, HostState>, entity: u64, in_ptr: u32| -> u32 {
                let wire = WireEntity::from_bits(entity);
                let Some(x) = read_xform(&mut caller, in_ptr) else {
                    return ErrorCode::InvalidArgument.as_u32();
                };
                match caller.data().set_xform(wire, &x) {
                    Ok(()) => ErrorCode::Ok.as_u32(),
                    Err(e) => e.as_u32(),
                }
            },
        )?;
    }

    // -- scene query --------------------------------------------------
    if caps.scene_query {
        linker.func_wrap(
            "idtx",
            "idtx_prim_custom_data",
            |mut caller: Caller<'_, HostState>,
             entity: u64,
             key_ptr: u32,
             key_len: u32,
             ptr: u32,
             cap: u32,
             len_ptr: u32|
             -> u32 {
                if key_len > 256 {
                    return ErrorCode::InvalidArgument.as_u32();
                }
                let Some(key) = read_string(&mut caller, key_ptr, key_len) else {
                    return ErrorCode::InvalidArgument.as_u32();
                };
                match caller
                    .data()
                    .custom_data(WireEntity::from_bits(entity), &key)
                {
                    Ok(json) => write_json_result(&mut caller, ptr, cap, len_ptr, &json),
                    Err(error) => error.as_u32(),
                }
            },
        )?;
        linker.func_wrap(
            "idtx",
            "idtx_entity_named_child",
            |mut caller: Caller<'_, HostState>,
             entity: u64,
             name_ptr: u32,
             name_len: u32,
             out_ptr: u32|
             -> u32 {
                if name_len > 256 {
                    return ErrorCode::InvalidArgument.as_u32();
                }
                let Some(name) = read_string(&mut caller, name_ptr, name_len) else {
                    return ErrorCode::InvalidArgument.as_u32();
                };
                match caller
                    .data()
                    .child_named(WireEntity::from_bits(entity), &name)
                {
                    Ok(child) => {
                        if write_checked(&mut caller, out_ptr, bytemuck::bytes_of(&child)) {
                            ErrorCode::Ok.as_u32()
                        } else {
                            ErrorCode::InvalidArgument.as_u32()
                        }
                    }
                    Err(error) => error.as_u32(),
                }
            },
        )?;
        linker.func_wrap(
            "idtx",
            "idtx_entity_find",
            |mut caller: Caller<'_, HostState>,
             path_ptr: u32,
             path_len: u32,
             out_ptr: u32|
             -> u32 {
                let Some(path) = read_string(&mut caller, path_ptr, path_len) else {
                    return ErrorCode::InvalidArgument.as_u32();
                };
                match caller.data().find(&path) {
                    Ok(wire) => {
                        write_entity(&mut caller, out_ptr, wire);
                        ErrorCode::Ok.as_u32()
                    }
                    Err(e) => e.as_u32(),
                }
            },
        )?;
        linker.func_wrap(
            "idtx",
            "idtx_entity_parent",
            |mut caller: Caller<'_, HostState>, entity: u64, out_ptr: u32| -> u32 {
                let wire = WireEntity::from_bits(entity);
                match caller.data().parent(wire) {
                    Ok(p) => {
                        write_entity(&mut caller, out_ptr, p);
                        ErrorCode::Ok.as_u32()
                    }
                    Err(e) => e.as_u32(),
                }
            },
        )?;
        linker.func_wrap(
            "idtx",
            "idtx_entity_child",
            |mut caller: Caller<'_, HostState>, entity: u64, index: u32, out_ptr: u32| -> u32 {
                let wire = WireEntity::from_bits(entity);
                match caller.data().child(wire, index) {
                    Ok(c) => {
                        write_entity(&mut caller, out_ptr, c);
                        ErrorCode::Ok.as_u32()
                    }
                    Err(e) => e.as_u32(),
                }
            },
        )?;
        linker.func_wrap(
            "idtx",
            "idtx_entity_child_count",
            |caller: Caller<'_, HostState>, entity: u64| -> u32 {
                caller.data().child_count(WireEntity::from_bits(entity))
            },
        )?;
        // Resolve a hit gprim up to its whole-asset collision body
        // (`IdtxCollisionBodyAPI`). Additive import (new slot), so old behaviour
        // wasm is unaffected and `ABI_VERSION` is unchanged. Writes `NULL` when
        // the entity is inside no body.
        linker.func_wrap(
            "idtx",
            "idtx_scene_collision_body",
            |mut caller: Caller<'_, HostState>, entity: u64, out_ptr: u32| -> u32 {
                let wire = WireEntity::from_bits(entity);
                match caller.data().scene_collision_body(wire) {
                    Ok(body) => {
                        write_entity(&mut caller, out_ptr, body);
                        ErrorCode::Ok.as_u32()
                    }
                    Err(e) => e.as_u32(),
                }
            },
        )?;
        // Read a prim's effective visibility (the composed ECS result the
        // renderer draws from). Additive import (new slot), so old behaviour
        // wasm is unaffected and `ABI_VERSION` is unchanged. Writes one byte
        // (`0` invisible / `1` visible) to `out_ptr`.
        linker.func_wrap(
            "idtx",
            "idtx_get_visibility",
            |mut caller: Caller<'_, HostState>, entity: u64, out_ptr: u32| -> u32 {
                let wire = WireEntity::from_bits(entity);
                match caller.data().visibility(wire) {
                    Ok(visible) => {
                        if write_checked(&mut caller, out_ptr, &[u8::from(visible)]) {
                            ErrorCode::Ok.as_u32()
                        } else {
                            ErrorCode::InvalidArgument.as_u32()
                        }
                    }
                    Err(e) => e.as_u32(),
                }
            },
        )?;

        // Scene interaction. Each reads a POD param struct from
        // guest memory, delegates to the installed `SceneBackend`, and writes a
        // POD result back. Variable-length output (the hit prim's path, the arc
        // polyline, the type-query list) uses the caller-buffer + true-length
        // convention of `idtx_event_current_name`.

        // Cast a ray: write `RayHitResult` to `out_ptr`, write ≤ `path_cap`
        // bytes of the hit prim's path to `path_out_ptr`, return the true path
        // byte length (0 on miss / no backend — check `RayHitResult.hit`).
        linker.func_wrap(
            "idtx",
            "idtx_raycast",
            |mut caller: Caller<'_, HostState>,
             params_ptr: u32,
             out_ptr: u32,
             path_out_ptr: u32,
             path_cap: u32|
             -> u32 {
                let Some(params) = read_pod::<RaycastParams>(&mut caller, params_ptr) else {
                    write_bytes(
                        &mut caller,
                        out_ptr,
                        bytemuck::bytes_of(&RayHitResult::default()),
                    );
                    return 0;
                };
                let Ok((hit, path)) = caller.data().raycast(&params) else {
                    write_bytes(
                        &mut caller,
                        out_ptr,
                        bytemuck::bytes_of(&RayHitResult::default()),
                    );
                    return 0;
                };
                write_bytes(&mut caller, out_ptr, bytemuck::bytes_of(&hit));
                write_path(&mut caller, path_out_ptr, path_cap, path.as_deref())
            },
        )?;

        // Cast a ballistic arc: fill up to `pts_cap` packed `[f32;3]` points at
        // `pts_out_ptr`, write `ArcHitResult` (its `sample_count` is the true
        // point count) to `out_ptr`, write the hit path to `path_out_ptr`, and
        // return the true path length.
        linker.func_wrap(
            "idtx",
            "idtx_arc_cast",
            |mut caller: Caller<'_, HostState>,
             params_ptr: u32,
             pts_out_ptr: u32,
             pts_cap: u32,
             out_ptr: u32,
             path_out_ptr: u32,
             path_cap: u32|
             -> u32 {
                let Some(params) = read_pod::<ArcCastParams>(&mut caller, params_ptr) else {
                    write_bytes(
                        &mut caller,
                        out_ptr,
                        bytemuck::bytes_of(&ArcHitResult::default()),
                    );
                    return 0;
                };
                // Clamp the guest-supplied capacity before allocating: `pts_cap`
                // is attacker-controlled (a hostile `.wasm` can pass `u32::MAX`),
                // and this is a HOST-side `Vec` outside the wasm StoreLimits / fuel
                // / epoch guards, so an unclamped `pts_cap` would let a sandboxed
                // behavior force a multi-gigabyte native allocation and OOM the
                // whole process. A ballistic-arc polyline is a small bounded sample
                // count in practice, so a fixed ceiling costs nothing. Mirrors how
                // net-guard / exec bound untrusted sizes with an explicit cap.
                let cap = (pts_cap as usize).min(MAX_ARC_SAMPLES);
                let mut points = vec![glam::Vec3::ZERO; cap];
                let Ok((hit, path)) = caller.data().arc_cast(&params, &mut points) else {
                    write_bytes(
                        &mut caller,
                        out_ptr,
                        bytemuck::bytes_of(&ArcHitResult::default()),
                    );
                    return 0;
                };
                let produced = (hit.sample_count as usize).min(cap);
                write_bytes(
                    &mut caller,
                    pts_out_ptr,
                    bytemuck::cast_slice(&points[..produced]),
                );
                write_bytes(&mut caller, out_ptr, bytemuck::bytes_of(&hit));
                write_path(&mut caller, path_out_ptr, path_cap, path.as_deref())
            },
        )?;

        // Run a prim-type query (`type_hash` = `abi::type_hash(token)`), caching
        // the matches host-side; returns the match count. Read each result back
        // with `idtx_type_query_get`.
        linker.func_wrap(
            "idtx",
            "idtx_find_by_type",
            |mut caller: Caller<'_, HostState>, type_hash: u64| -> u32 {
                caller.data_mut().find_by_type(type_hash).unwrap_or(0)
            },
        )?;

        // Read the `index`-th cached type-query result: write its entity handle
        // to `out_entity_ptr`, ≤ `path_cap` path bytes to `path_out_ptr`, and
        // return the true path length (`u32::MAX` when `index` is out of range).
        linker.func_wrap(
            "idtx",
            "idtx_type_query_get",
            |mut caller: Caller<'_, HostState>,
             index: u32,
             out_entity_ptr: u32,
             path_out_ptr: u32,
             path_cap: u32|
             -> u32 {
                let Some((entity, path)) = caller
                    .data()
                    .type_query_get(index)
                    .map(|(e, p)| (e, p.to_owned()))
                else {
                    return u32::MAX;
                };
                write_entity(&mut caller, out_entity_ptr, entity);
                write_path(&mut caller, path_out_ptr, path_cap, Some(&path))
            },
        )?;
    }

    // -- input --------------------------------------------------------
    if caps.input {
        linker.func_wrap(
            "idtx",
            "idtx_input_resolve",
            |mut caller: Caller<'_, HostState>, name_ptr: u32, name_len: u32| -> u32 {
                let Some(name) = read_string(&mut caller, name_ptr, name_len) else {
                    return 0;
                };
                caller.data().action_ids.get(&name).copied().unwrap_or(0)
            },
        )?;
        linker.func_wrap(
            "idtx",
            "idtx_input_action_phase",
            |caller: Caller<'_, HostState>, id: u32| -> u32 {
                caller
                    .data()
                    .input
                    .phase(idtx_behavior_abi::InputActionId(id))
                    .as_u32()
            },
        )?;
        linker.func_wrap(
            "idtx",
            "idtx_input_axis",
            |caller: Caller<'_, HostState>, id: u32| -> f32 {
                caller
                    .data()
                    .input
                    .axis(idtx_behavior_abi::InputActionId(id))
            },
        )?;
        linker.func_wrap(
            "idtx",
            "idtx_input_axis2",
            |mut caller: Caller<'_, HostState>, id: u32, out_ptr: u32| -> u32 {
                let v = caller
                    .data()
                    .input
                    .axis2(idtx_behavior_abi::InputActionId(id));
                write_f32x2(&mut caller, out_ptr, v);
                ErrorCode::Ok.as_u32()
            },
        )?;
        linker.func_wrap(
            "idtx",
            "idtx_input_mouse_delta",
            |mut caller: Caller<'_, HostState>, out_ptr: u32| {
                let d = caller.data().input.mouse_delta;
                write_f32x2(&mut caller, out_ptr, d);
            },
        )?;
        linker.func_wrap(
            "idtx",
            "idtx_input_scroll",
            |caller: Caller<'_, HostState>| -> f32 { caller.data().input.scroll },
        )?;
    }

    // -- xr_hmd -------------------------------------------
    if caps.xr_hmd {
        linker.func_wrap(
            "idtx",
            "idtx_xr_get_pose",
            |mut caller: Caller<'_, HostState>, space: u32, out_ptr: u32| -> u32 {
                let pose = caller.data().xr_pose(XrSpace::from_u32(space));
                write_pose(&mut caller, out_ptr, &pose);
                ErrorCode::Ok.as_u32()
            },
        )?;
        // Play-space recenter offset: write the stage-space ground vector (12
        // bytes, `abi::Vec3`) the host wants the pawn re-based by. Always Ok
        // (`ZERO` when inactive). Gated with `xr_hmd` — HMD-derived data.
        linker.func_wrap(
            "idtx",
            "idtx_xr_get_recenter",
            |mut caller: Caller<'_, HostState>, out_ptr: u32| -> u32 {
                let r = WireVec3::from(caller.data().xr_recenter());
                write_bytes(&mut caller, out_ptr, bytemuck::bytes_of(&r));
                ErrorCode::Ok.as_u32()
            },
        )?;
        // Write 26 × XrHandJointResult (1040 bytes) to `out_ptr`.
        // Returns NotFound when the hand is not tracked this frame.
        linker.func_wrap(
            "idtx",
            "idtx_xr_get_hand_joints",
            |mut caller: Caller<'_, HostState>, side: u32, out_ptr: u32| -> u32 {
                let side = XrSide::from_u32(side);
                let joints = caller.data().xr_hand_joints(side).copied();
                let Some(joints) = joints else {
                    return ErrorCode::NotFound.as_u32();
                };
                let bytes = bytemuck::cast_slice::<XrHandJointResult, u8>(&joints);
                write_bytes(&mut caller, out_ptr, bytes);
                ErrorCode::Ok.as_u32()
            },
        )?;
        // Which input source(s) drive `side` this frame, as an `XrInputSource`
        // bitmask (CONTROLLER | HAND) returned directly. Lets a behavior tell a
        // controller from hand tracking even when the grip/aim pose spaces are
        // synthesised from the hand. `NONE` (0) when the side is not tracked.
        linker.func_wrap(
            "idtx",
            "idtx_xr_get_tracking_mode",
            |caller: Caller<'_, HostState>, side: u32| -> u32 {
                caller.data().xr_input_source(XrSide::from_u32(side)).bits()
            },
        )?;
    }

    // -- events -------------------------------------------------------
    if caps.events {
        linker.func_wrap(
            "idtx",
            "idtx_event_post",
            |mut caller: Caller<'_, HostState>,
             target: u64,
             name_ptr: u32,
             name_len: u32,
             payload_ptr: u32,
             payload_len: u32|
             -> u32 {
                let Some(name) = read_string(&mut caller, name_ptr, name_len) else {
                    return ErrorCode::InvalidArgument.as_u32();
                };
                let Some(payload) = read_bytes(&mut caller, payload_ptr, payload_len) else {
                    return ErrorCode::InvalidArgument.as_u32();
                };
                match caller
                    .data_mut()
                    .post_event(WireEntity::from_bits(target), name, payload)
                {
                    Ok(()) => ErrorCode::Ok.as_u32(),
                    Err(e) => e.as_u32(),
                }
            },
        )?;

        // Read the event currently being delivered to `on_event`. The
        // dispatcher seeds it before the hook fires; outside `on_event`
        // these return an empty/null event. The `_name`/`_payload` copies
        // write up to `cap` bytes to `out_ptr` and return the event's
        // *true* length (so a guest can detect truncation).
        linker.func_wrap(
            "idtx",
            "idtx_event_current_target",
            |caller: Caller<'_, HostState>| -> u64 { caller.data().event_target().to_bits() },
        )?;
        linker.func_wrap(
            "idtx",
            "idtx_event_current_name",
            |mut caller: Caller<'_, HostState>, out_ptr: u32, cap: u32| -> u32 {
                let name = caller
                    .data()
                    .event_name()
                    .map(str::to_owned)
                    .unwrap_or_default();
                let bytes = name.as_bytes();
                let n = (u32::try_from(bytes.len()).unwrap_or(u32::MAX)).min(cap) as usize;
                write_bytes(&mut caller, out_ptr, &bytes[..n]);
                u32::try_from(bytes.len()).unwrap_or(u32::MAX)
            },
        )?;
        linker.func_wrap(
            "idtx",
            "idtx_event_current_payload",
            |mut caller: Caller<'_, HostState>, out_ptr: u32, cap: u32| -> u32 {
                let payload = caller
                    .data()
                    .event_payload()
                    .map(<[u8]>::to_vec)
                    .unwrap_or_default();
                let n = (u32::try_from(payload.len()).unwrap_or(u32::MAX)).min(cap) as usize;
                write_bytes(&mut caller, out_ptr, &payload[..n]);
                u32::try_from(payload.len()).unwrap_or(u32::MAX)
            },
        )?;
    }

    // -- widget interaction (ABI 3) -----------------------------------
    //
    // The event-READ trio is ALWAYS LINKED (no capability): the macro-generated
    // `_idtx_on_event` dispatcher in every behavior calls `idtx_widget_event_kind`
    // to tell a widget event from a regular one, so it must link regardless of
    // grants. The privileged `register_handler` / `handle` are gated on `widget`.
    linker.func_wrap(
        "idtx",
        "idtx_widget_event_kind",
        |caller: Caller<'_, HostState>| -> u32 { caller.data().widget_event_kind() },
    )?;
    linker.func_wrap(
        "idtx",
        "idtx_widget_event_element",
        |mut caller: Caller<'_, HostState>, out_ptr: u32, cap: u32| -> u32 {
            let elem = caller
                .data()
                .widget_event_element()
                .map(str::to_owned)
                .unwrap_or_default();
            let bytes = elem.as_bytes();
            let n = (u32::try_from(bytes.len()).unwrap_or(u32::MAX)).min(cap) as usize;
            write_bytes(&mut caller, out_ptr, &bytes[..n]);
            u32::try_from(bytes.len()).unwrap_or(u32::MAX)
        },
    )?;
    linker.func_wrap(
        "idtx",
        "idtx_widget_event_uv",
        |mut caller: Caller<'_, HostState>, out_ptr: u32| {
            let uv = caller.data().widget_event_uv();
            write_f32x2(&mut caller, out_ptr, uv);
        },
    )?;
    if caps.widget {
        linker.func_wrap(
            "idtx",
            "idtx_widget_set_value",
            |mut caller: Caller<'_, HostState>,
             name_ptr: u32,
             name_len: u32,
             value_ptr: u32,
             value_len: u32|
             -> u32 {
                if name_len == 0 || name_len > 128 || value_len > 1024 {
                    return ErrorCode::InvalidArgument.as_u32();
                }
                let (Some(name), Some(value)) = (
                    read_string(&mut caller, name_ptr, name_len),
                    read_string(&mut caller, value_ptr, value_len),
                ) else {
                    return ErrorCode::InvalidArgument.as_u32();
                };
                let values = &mut caller.data_mut().widget_values;
                if !values.contains_key(&name) && values.len() >= 16 {
                    return ErrorCode::InvalidArgument.as_u32();
                }
                if value.is_empty() {
                    values.remove(&name);
                } else {
                    values.insert(name, value);
                }
                ErrorCode::Ok.as_u32()
            },
        )?;
        linker.func_wrap(
            "idtx",
            "idtx_widget_get_value",
            |mut caller: Caller<'_, HostState>,
             name_ptr: u32,
             name_len: u32,
             out_ptr: u32,
             cap: u32|
             -> u32 {
                if name_len > 128 || cap > 1024 {
                    return u32::MAX;
                }
                let Some(name) = read_string(&mut caller, name_ptr, name_len) else {
                    return u32::MAX;
                };
                let value = caller
                    .data()
                    .widget_values
                    .get(&name)
                    .cloned()
                    .unwrap_or_default();
                write_path(&mut caller, out_ptr, cap, Some(&value))
            },
        )?;
        linker.func_wrap(
            "idtx",
            "idtx_widget_register_handler",
            |mut caller: Caller<'_, HostState>,
             panel: u64,
             path_ptr: u32,
             path_len: u32,
             kind: u32|
             -> u32 {
                let Some(path) = read_string(&mut caller, path_ptr, path_len) else {
                    return ErrorCode::InvalidArgument.as_u32();
                };
                match caller.data_mut().register_widget_handler(
                    WireEntity::from_bits(panel),
                    &path,
                    idtx_behavior_abi::WidgetEventKind::from_u32(kind),
                ) {
                    Ok(()) => ErrorCode::Ok.as_u32(),
                    Err(e) => e.as_u32(),
                }
            },
        )?;
        linker.func_wrap(
            "idtx",
            "idtx_widget_handle",
            |mut caller: Caller<'_, HostState>, entity: u64, px: f32, py: f32, pz: f32| -> u32 {
                match caller
                    .data_mut()
                    .widget_handle(WireEntity::from_bits(entity), glam::Vec3::new(px, py, pz))
                {
                    Ok(true) => 1,
                    Ok(false) | Err(_) => 0,
                }
            },
        )?;
    }

    // -- playback ----------------------------------------------
    if caps.playback {
        linker.func_wrap(
            "idtx",
            "idtx_playback_command",
            |mut caller: Caller<'_, HostState>,
             target: u64,
             clip_ptr: u32,
             clip_len: u32,
             kind: u32,
             value: f32|
             -> u32 {
                let Some(clip) = read_string(&mut caller, clip_ptr, clip_len) else {
                    return ErrorCode::InvalidArgument.as_u32();
                };
                match caller.data_mut().post_playback(
                    WireEntity::from_bits(target),
                    clip,
                    PlaybackCommandKind::from_u32(kind),
                    value,
                ) {
                    Ok(()) => ErrorCode::Ok.as_u32(),
                    Err(e) => e.as_u32(),
                }
            },
        )?;
    }

    // -- session-layer authoring -------------------------------
    // Guests with `stage_edit` may post deferred edits against the session
    // layer. Nothing is applied in-host: the write is enqueued and the
    // session drains + applies + re-projects after the update hooks (USD is
    // the single source of truth). Imports are absent without the grant, so
    // an ungranted module fails to link.
    if caps.stage_edit {
        linker.func_wrap(
            "idtx",
            "idtx_session_spawn_reference",
            |mut caller: Caller<'_, HostState>,
             parent: u64,
             name_ptr: u32,
             name_len: u32,
             prototype_ptr: u32,
             prototype_len: u32,
             xform_ptr: u32,
             payload_ptr: u32,
             payload_len: u32,
             ticket_ptr: u32|
             -> u32 {
                if name_len > 256
                    || prototype_len > 256
                    || payload_len as usize > idtx_behavior_abi::MAX_SPAWN_PAYLOAD_BYTES
                    || !range_valid(&mut caller, ticket_ptr, 8)
                {
                    return ErrorCode::InvalidArgument.as_u32();
                }
                let Some(name) = read_string(&mut caller, name_ptr, name_len) else {
                    return ErrorCode::InvalidArgument.as_u32();
                };
                let Some(prototype) = read_string(&mut caller, prototype_ptr, prototype_len) else {
                    return ErrorCode::InvalidArgument.as_u32();
                };
                let Some(transform) = read_xform(&mut caller, xform_ptr) else {
                    return ErrorCode::InvalidArgument.as_u32();
                };
                let Some(payload) = read_string(&mut caller, payload_ptr, payload_len) else {
                    return ErrorCode::InvalidArgument.as_u32();
                };
                let (context, connections) = match crate::spawn::decode_payload(&payload) {
                    Ok(decoded) => decoded,
                    Err(error) => return error.as_u32(),
                };
                match caller.data_mut().post_reference_spawn(
                    WireEntity::from_bits(parent),
                    name,
                    prototype,
                    transform,
                    context,
                    connections,
                ) {
                    Ok(ticket) => {
                        if write_checked(&mut caller, ticket_ptr, bytemuck::bytes_of(&ticket)) {
                            ErrorCode::Ok.as_u32()
                        } else {
                            ErrorCode::InvalidArgument.as_u32()
                        }
                    }
                    Err(error) => error.as_u32(),
                }
            },
        )?;
        linker.func_wrap(
            "idtx",
            "idtx_session_spawn_status",
            |mut caller: Caller<'_, HostState>, ticket: u64, out_ptr: u32| -> u32 {
                if !range_valid(&mut caller, out_ptr, 16) {
                    return ErrorCode::InvalidArgument.as_u32();
                }
                match caller.data().spawn_result(ticket) {
                    Ok(result) => {
                        if !write_checked(&mut caller, out_ptr, bytemuck::bytes_of(&result)) {
                            return ErrorCode::InvalidArgument.as_u32();
                        }
                        caller.data_mut().consume_spawn_result(ticket);
                        ErrorCode::Ok.as_u32()
                    }
                    Err(error) => error.as_u32(),
                }
            },
        )?;
        linker.func_wrap(
            "idtx",
            "idtx_session_set_xform",
            |mut caller: Caller<'_, HostState>, target: u64, in_ptr: u32| -> u32 {
                let Some(xform) = read_xform(&mut caller, in_ptr) else {
                    return ErrorCode::InvalidArgument.as_u32();
                };
                match caller.data_mut().post_session_edit(
                    WireEntity::from_bits(target),
                    SessionEditOp::SetTransform(xform),
                ) {
                    Ok(()) => ErrorCode::Ok.as_u32(),
                    Err(e) => e.as_u32(),
                }
            },
        )?;
        linker.func_wrap(
            "idtx",
            "idtx_session_set_visibility",
            |mut caller: Caller<'_, HostState>, target: u64, visible: u32| -> u32 {
                match caller.data_mut().post_session_edit(
                    WireEntity::from_bits(target),
                    SessionEditOp::SetVisibility(visible != 0),
                ) {
                    Ok(()) => ErrorCode::Ok.as_u32(),
                    Err(e) => e.as_u32(),
                }
            },
        )?;
        // Dynamic prim inject/remove. `define_prim` targets the parent
        // (a new child `name` of type `type_name` is authored under it);
        // `remove_prim` targets the prim to delete. Both route through the
        // session's structural (TOPOLOGY) apply path on drain.
        linker.func_wrap(
            "idtx",
            "idtx_session_define_prim",
            |mut caller: Caller<'_, HostState>,
             parent: u64,
             type_ptr: u32,
             type_len: u32,
             name_ptr: u32,
             name_len: u32|
             -> u32 {
                let Some(type_name) = read_string(&mut caller, type_ptr, type_len) else {
                    return ErrorCode::InvalidArgument.as_u32();
                };
                let Some(name) = read_string(&mut caller, name_ptr, name_len) else {
                    return ErrorCode::InvalidArgument.as_u32();
                };
                if name.is_empty() {
                    return ErrorCode::InvalidArgument.as_u32();
                }
                match caller.data_mut().post_session_edit(
                    WireEntity::from_bits(parent),
                    SessionEditOp::DefinePrim { type_name, name },
                ) {
                    Ok(()) => ErrorCode::Ok.as_u32(),
                    Err(e) => e.as_u32(),
                }
            },
        )?;
        linker.func_wrap(
            "idtx",
            "idtx_session_remove_prim",
            |mut caller: Caller<'_, HostState>, target: u64| -> u32 {
                match caller
                    .data_mut()
                    .post_session_edit(WireEntity::from_bits(target), SessionEditOp::RemovePrim)
                {
                    Ok(()) => ErrorCode::Ok.as_u32(),
                    Err(e) => e.as_u32(),
                }
            },
        )?;
        // Attach a screen-space HUD (e.g. a center crosshair) to the target
        // (typically the player camera): atomically define an `IdtxHud` child
        // `name` into the session layer and author its parameters there. One op
        // because the SDK cannot hand back the new prim's entity id in-frame.
        // `size_px` is the on-screen pixel size (0,0 = native image size).
        linker.func_wrap(
            "idtx",
            "idtx_hud_attach",
            |mut caller: Caller<'_, HostState>,
             target: u64,
             name_ptr: u32,
             name_len: u32,
             image_ptr: u32,
             image_len: u32,
             size_w: i32,
             size_h: i32,
             opacity: f32|
             -> u32 {
                let Some(name) = read_string(&mut caller, name_ptr, name_len) else {
                    return ErrorCode::InvalidArgument.as_u32();
                };
                let Some(image) = read_string(&mut caller, image_ptr, image_len) else {
                    return ErrorCode::InvalidArgument.as_u32();
                };
                if name.is_empty() || image.is_empty() {
                    return ErrorCode::InvalidArgument.as_u32();
                }
                match caller.data_mut().post_session_edit(
                    WireEntity::from_bits(target),
                    SessionEditOp::AttachHud {
                        name,
                        image,
                        size_px: [size_w, size_h],
                        opacity,
                    },
                ) {
                    Ok(()) => ErrorCode::Ok.as_u32(),
                    Err(e) => e.as_u32(),
                }
            },
        )?;

        // Author a string into an `IdtxWidgetField*` child's `idtx:widget:value`
        // (session layer). The deferred op carries the panel-relative child path
        // and the string; the session's apply site joins `<target>/<name>` and
        // authors it, driving the widget re-hydrate + render patch on drain.
        linker.func_wrap(
            "idtx",
            "idtx_session_set_widget_value",
            |mut caller: Caller<'_, HostState>,
             target: u64,
             name_ptr: u32,
             name_len: u32,
             value_ptr: u32,
             value_len: u32|
             -> u32 {
                let Some(name) = read_string(&mut caller, name_ptr, name_len) else {
                    return ErrorCode::InvalidArgument.as_u32();
                };
                let Some(value) = read_string(&mut caller, value_ptr, value_len) else {
                    return ErrorCode::InvalidArgument.as_u32();
                };
                if name.is_empty() {
                    return ErrorCode::InvalidArgument.as_u32();
                }
                match caller.data_mut().post_session_edit(
                    WireEntity::from_bits(target),
                    SessionEditOp::SetWidgetValue { name, value },
                ) {
                    Ok(()) => ErrorCode::Ok.as_u32(),
                    Err(e) => e.as_u32(),
                }
            },
        )?;
    }

    // -- overlay / render commands -----------------------------
    // Guests with `overlay` may post transient highlight / material-override /
    // post-process commands. Nothing is applied in-host: the command is
    // enqueued and the session drains it after the update hooks and feeds the
    // render world (never authored into USD). Absent without the grant, so an
    // ungranted module fails to link.
    if caps.overlay {
        linker.func_wrap(
            "idtx",
            "idtx_overlay_command",
            |mut caller: Caller<'_, HostState>,
             kind: u32,
             target: u64,
             a_ptr: u32,
             a_len: u32,
             b_ptr: u32,
             b_len: u32,
             color_ptr: u32,
             scalar: f32|
             -> u32 {
                let _ = b_len; // reserved for a future string `b` argument
                let op = match OverlayCommandKind::from_u32(kind) {
                    OverlayCommandKind::DrawLine => OverlayOp::Line {
                        start: read_vec3(&mut caller, a_ptr),
                        end: read_vec3(&mut caller, b_ptr),
                        color: read_vec4(&mut caller, color_ptr),
                        width: scalar,
                    },
                    OverlayCommandKind::DrawArc => OverlayOp::Arc {
                        start: read_vec3(&mut caller, a_ptr),
                        dir: read_vec3(&mut caller, b_ptr),
                        speed: scalar,
                        gravity: 9.81,
                        color: read_vec4(&mut caller, color_ptr),
                        width: 0.02,
                    },
                    OverlayCommandKind::Highlight => OverlayOp::Highlight {
                        color: read_vec4(&mut caller, color_ptr),
                    },
                    OverlayCommandKind::ClearHighlight => OverlayOp::ClearHighlight,
                    OverlayCommandKind::HighlightTint => OverlayOp::HighlightTint {
                        color: read_vec4(&mut caller, color_ptr),
                    },
                    OverlayCommandKind::HighlightOutline => OverlayOp::HighlightOutline {
                        color: read_vec4(&mut caller, color_ptr),
                        thickness_px: scalar,
                    },
                    OverlayCommandKind::SetMaterialOverride => {
                        let Some(material) = read_string(&mut caller, a_ptr, a_len) else {
                            return ErrorCode::InvalidArgument.as_u32();
                        };
                        OverlayOp::SetMaterialOverride { material }
                    }
                    OverlayCommandKind::ClearMaterialOverride => OverlayOp::ClearMaterialOverride,
                    OverlayCommandKind::SetPostprocessParam => {
                        let Some(name) = read_string(&mut caller, a_ptr, a_len) else {
                            return ErrorCode::InvalidArgument.as_u32();
                        };
                        OverlayOp::SetPostprocessParam {
                            name,
                            value: scalar,
                        }
                    }
                };
                match caller
                    .data_mut()
                    .post_overlay(WireEntity::from_bits(target), op)
                {
                    Ok(()) => ErrorCode::Ok.as_u32(),
                    Err(e) => e.as_u32(),
                }
            },
        )?;
    }

    // -- physics control (grab suspend/resume) ------------------------
    // Guests with `physics` may suspend/resume simulation of a specific body.
    // Nothing is applied in-host: the command is enqueued and the session drains
    // it after the update hooks and applies it to its physics subsystem (before
    // the physics tick). Absent without the grant, so an ungranted module fails
    // to link. Additive import slot — `ABI_VERSION` is unchanged.
    if caps.physics {
        linker.func_wrap(
            "idtx",
            "idtx_physics_set_simulated",
            |mut caller: Caller<'_, HostState>, target: u64, enabled: u32| -> u32 {
                match caller
                    .data_mut()
                    .post_physics_control(WireEntity::from_bits(target), enabled != 0)
                {
                    Ok(()) => ErrorCode::Ok.as_u32(),
                    Err(e) => e.as_u32(),
                }
            },
        )?;
        // Resolve a hit child collider up to its rigid-body root
        // (`UsdPhysicsRigidBodyAPI`) so a grab suspends/resumes the whole
        // simulated body. Additive import slot — `ABI_VERSION` unchanged. Writes
        // `NULL` when the entity is inside no rigid body.
        linker.func_wrap(
            "idtx",
            "idtx_scene_physics_body",
            |mut caller: Caller<'_, HostState>, entity: u64, out_ptr: u32| -> u32 {
                let wire = WireEntity::from_bits(entity);
                match caller.data().scene_physics_body(wire) {
                    Ok(body) => {
                        write_entity(&mut caller, out_ptr, body);
                        ErrorCode::Ok.as_u32()
                    }
                    Err(e) => e.as_u32(),
                }
            },
        )?;
    }

    // -- level_control (request a runtime level transition) -----------
    // Guests with `level_control` may ask the runtime to travel to another
    // level. Nothing is applied in-host: the request is enqueued and the session
    // surfaces it after the update hooks for the runtime driver to poll,
    // which performs the actual session swap — never from inside this import.
    // Absent without the grant, so an ungranted module fails to link. Additive
    // import slot — `ABI_VERSION` is unchanged.
    if caps.level_control {
        linker.func_wrap(
            "idtx",
            "idtx_cancel_level_transition",
            |mut caller: Caller<'_, HostState>| -> u32 {
                caller.data_mut().cancel_level_transition = true;
                ErrorCode::Ok.as_u32()
            },
        )?;
        linker.func_wrap(
            "idtx",
            "idtx_state_set",
            |mut caller: Caller<'_, HostState>,
             key_ptr: u32,
             key_len: u32,
             value_ptr: u32,
             value_len: u32|
             -> u32 {
                if key_len as usize > crate::experience::MAX_KEY_BYTES
                    || value_len as usize > crate::experience::MAX_VALUE_BYTES
                {
                    return ErrorCode::InvalidArgument.as_u32();
                }
                let (Some(key), Some(value)) = (
                    read_string(&mut caller, key_ptr, key_len),
                    read_bytes(&mut caller, value_ptr, value_len),
                ) else {
                    return ErrorCode::InvalidArgument.as_u32();
                };
                caller
                    .data()
                    .experience_state
                    .set(key, value)
                    .map_or_else(ErrorCode::as_u32, |()| ErrorCode::Ok.as_u32())
            },
        )?;
        linker.func_wrap(
            "idtx",
            "idtx_state_get",
            |mut caller: Caller<'_, HostState>,
             key_ptr: u32,
             key_len: u32,
             out_ptr: u32,
             capacity: u32,
             length_ptr: u32|
             -> u32 {
                if key_len == 0 || key_len as usize > crate::experience::MAX_KEY_BYTES {
                    return ErrorCode::InvalidArgument.as_u32();
                }
                let Some(key) = read_string(&mut caller, key_ptr, key_len) else {
                    return ErrorCode::InvalidArgument.as_u32();
                };
                let Some(value) = caller.data().experience_state.get(&key) else {
                    return ErrorCode::NotFound.as_u32();
                };
                let Some(mem) = memory(&mut caller) else {
                    return ErrorCode::InvalidArgument.as_u32();
                };
                let size = mem.data_size(&caller);
                if value.len() > capacity as usize
                    || (out_ptr as usize)
                        .checked_add(value.len())
                        .is_none_or(|end| end > size)
                    || (length_ptr as usize)
                        .checked_add(4)
                        .is_none_or(|end| end > size)
                {
                    return ErrorCode::InvalidArgument.as_u32();
                }
                let Ok(length) = u32::try_from(value.len()) else {
                    return ErrorCode::InvalidArgument.as_u32();
                };
                if mem.write(&mut caller, out_ptr as usize, &value).is_err()
                    || mem
                        .write(&mut caller, length_ptr as usize, &length.to_le_bytes())
                        .is_err()
                {
                    return ErrorCode::InvalidArgument.as_u32();
                }
                ErrorCode::Ok.as_u32()
            },
        )?;
        linker.func_wrap(
            "idtx",
            "idtx_request_level_transition",
            |mut caller: Caller<'_, HostState>,
             to_ptr: u32,
             to_len: u32,
             spawn_ptr: u32,
             spawn_len: u32|
             -> u32 {
                let Some(target) = read_string(&mut caller, to_ptr, to_len) else {
                    return ErrorCode::InvalidArgument.as_u32();
                };
                if target.is_empty() {
                    return ErrorCode::InvalidArgument.as_u32();
                }
                // A zero-length spawn means "the target's default spawn" (None);
                // a non-empty spawn is read as a named spawn point.
                let spawn = if spawn_len == 0 {
                    None
                } else {
                    let Some(spawn) = read_string(&mut caller, spawn_ptr, spawn_len) else {
                        return ErrorCode::InvalidArgument.as_u32();
                    };
                    Some(spawn)
                };
                match caller.data_mut().post_level_transition(target, spawn) {
                    Ok(()) => ErrorCode::Ok.as_u32(),
                    Err(e) => e.as_u32(),
                }
            },
        )?;
    }

    // -- auth_write (write credentials for later use) ---------
    // Write-only, within the embedding host's explicit origin group. All HTTP
    // consumers resolve these grants by destination URL; no unscoped fallback.
    if caps.auth_write {
        linker.func_wrap(
            "idtx",
            "idtx_auth_set_token",
            |mut caller: Caller<'_, HostState>, ptr: u32, len: u32| -> u32 {
                match read_string(&mut caller, ptr, len) {
                    Some(token) => {
                        if caller.data().auth_scopes.is_empty() {
                            return ErrorCode::CapabilityDenied.as_u32();
                        }
                        auth_result(caller.data().auth_scopes.set_bearer_token(token))
                    }
                    None => ErrorCode::InvalidArgument.as_u32(),
                }
            },
        )?;
        linker.func_wrap(
            "idtx",
            "idtx_auth_set_usercred",
            |mut caller: Caller<'_, HostState>,
             user_ptr: u32,
             user_len: u32,
             pass_ptr: u32,
             pass_len: u32|
             -> u32 {
                let (Some(user), Some(password)) = (
                    read_string(&mut caller, user_ptr, user_len),
                    read_string(&mut caller, pass_ptr, pass_len),
                ) else {
                    return ErrorCode::InvalidArgument.as_u32();
                };
                if caller.data().auth_scopes.is_empty() {
                    return ErrorCode::CapabilityDenied.as_u32();
                }
                auth_result(
                    caller
                        .data()
                        .auth_scopes
                        .set_user_credentials(user, password),
                )
            },
        )?;
        linker.func_wrap(
            "idtx",
            "idtx_auth_clear",
            |caller: Caller<'_, HostState>| -> u32 {
                auth_result(caller.data().auth_scopes.clear())
            },
        )?;
        linker.func_wrap(
            "idtx",
            "idtx_auth_revision",
            |_caller: Caller<'_, HostState>| -> u64 { idtx_auth::revision() },
        )?;
        // Scoped variants accept comma-separated origin selectors within the
        // configured group. Legacy bare authorities mean HTTPS; a caller must
        // spell HTTP or a non-default port explicitly.
        linker.func_wrap(
            "idtx",
            "idtx_auth_set_token_for_host",
            |mut caller: Caller<'_, HostState>,
             host_ptr: u32,
             host_len: u32,
             tok_ptr: u32,
             tok_len: u32|
             -> u32 {
                let (Some(hosts), Some(token)) = (
                    read_string(&mut caller, host_ptr, host_len),
                    read_string(&mut caller, tok_ptr, tok_len),
                ) else {
                    return ErrorCode::InvalidArgument.as_u32();
                };
                let hosts = split_host_list(&hosts);
                if hosts.is_empty() {
                    return ErrorCode::InvalidArgument.as_u32();
                }
                let Ok(scopes) = idtx_auth::AuthScopes::from_host_selectors(&hosts) else {
                    return ErrorCode::InvalidArgument.as_u32();
                };
                if !caller.data().auth_scopes.permits(&scopes) {
                    return ErrorCode::CapabilityDenied.as_u32();
                }
                auth_result(scopes.set_bearer_token(token))
            },
        )?;
        linker.func_wrap(
            "idtx",
            "idtx_auth_set_usercred_for_host",
            |mut caller: Caller<'_, HostState>,
             host_ptr: u32,
             host_len: u32,
             user_ptr: u32,
             user_len: u32,
             pass_ptr: u32,
             pass_len: u32|
             -> u32 {
                let (Some(hosts), Some(user), Some(password)) = (
                    read_string(&mut caller, host_ptr, host_len),
                    read_string(&mut caller, user_ptr, user_len),
                    read_string(&mut caller, pass_ptr, pass_len),
                ) else {
                    return ErrorCode::InvalidArgument.as_u32();
                };
                let hosts = split_host_list(&hosts);
                if hosts.is_empty() {
                    return ErrorCode::InvalidArgument.as_u32();
                }
                let Ok(scopes) = idtx_auth::AuthScopes::from_host_selectors(&hosts) else {
                    return ErrorCode::InvalidArgument.as_u32();
                };
                if !caller.data().auth_scopes.permits(&scopes) {
                    return ErrorCode::CapabilityDenied.as_u32();
                }
                auth_result(scopes.set_user_credentials(user, password))
            },
        )?;
    }

    // -- net (HTTP egress) --------------------------------------------
    // Async HTTP uses the SAME deny-by-default SSRF policy as OpenExec REST
    // datasources (shared `idtx_net_guard` gate): the request is
    // validated (allowlisted host, http/https, resolves only to public addresses)
    // and issued through a redirect-disabled, DNS-pinned client, with the body
    // capped at the policy's `max_response_bytes`. The policy is carried per
    // instance in `HostState` (installed by the session from its `[network]`
    // config); without a configured allowlist every request is rejected.
    //
    // Gated behind the crate `net` feature so a host that opts out links no HTTP
    // stack — the import is then simply absent and a `net`-capability behavior
    // fails to link (capability-denied), exactly like any ungranted bucket.
    #[cfg(feature = "net")]
    if caps.net {
        use idtx_behavior_abi::HttpResult;
        // Keep the legacy ABI slot linkable, but reject blocking egress before
        // reading arguments or doing DNS/I/O. Fuel/epochs cannot preempt a host
        // import. Guest network work must use the bounded asynchronous path.
        linker.func_wrap(
            "idtx",
            "idtx_http_request",
            |mut caller: Caller<'_, HostState>,
             _method_ptr: u32,
             _method_len: u32,
             _url_ptr: u32,
             _url_len: u32,
             _ctype_ptr: u32,
             _ctype_len: u32,
             _body_ptr: u32,
             _body_len: u32,
             _resp_out_ptr: u32,
             _resp_cap: u32,
             result_out_ptr: u32|
             -> u32 {
                write_bytes(
                    &mut caller,
                    result_out_ptr,
                    bytemuck::bytes_of(&HttpResult::default()),
                );
                ErrorCode::CapabilityDenied.as_u32()
            },
        )?;

        // Asynchronous (non-blocking) egress. The guest supplies a `request_id`
        // (its own correlation token) and this returns immediately: the request
        // is validated + issued on a background worker, which pushes an
        // `HttpCompletion` into the instance's `http_inbox`. The host drains the
        // inbox each `run_frame` and delivers the result via the guest's
        // `_idtx_on_http_response(request_id, status)` export — so the hook never
        // blocks and the wall-clock budget is never blown. All validation (incl.
        // blocking DNS) runs on the worker; a denial surfaces as a `status == 0`
        // completion. Rejected synchronously (without spawning) when the method
        // is malformed or too many requests are already in flight.
        linker.func_wrap(
            "idtx",
            "idtx_http_request_async",
            |mut caller: Caller<'_, HostState>,
             request_id: u32,
             method_ptr: u32,
             method_len: u32,
             url_ptr: u32,
             url_len: u32,
             ctype_ptr: u32,
             ctype_len: u32,
             body_ptr: u32,
             body_len: u32|
             -> u32 {
                let (Some(method), Some(url)) = (
                    read_string(&mut caller, method_ptr, method_len),
                    read_string(&mut caller, url_ptr, url_len),
                ) else {
                    return ErrorCode::InvalidArgument.as_u32();
                };
                let (Some(content_type), Some(body)) = (
                    read_string(&mut caller, ctype_ptr, ctype_len),
                    read_bytes(&mut caller, body_ptr, body_len),
                ) else {
                    return ErrorCode::InvalidArgument.as_u32();
                };
                let Ok(method) = reqwest::Method::from_bytes(method.as_bytes()) else {
                    return ErrorCode::InvalidArgument.as_u32();
                };

                // Cap in-flight requests so a behavior firing every frame cannot
                // spawn unbounded worker threads.
                let outstanding = caller.data().http_outstanding.clone();
                if outstanding.load(std::sync::atomic::Ordering::Relaxed) >= MAX_OUTSTANDING_HTTP {
                    tracing::warn!(
                        request_id,
                        max = MAX_OUTSTANDING_HTTP,
                        "behavior net: too many in-flight async requests; rejecting"
                    );
                    return ErrorCode::InvalidArgument.as_u32();
                }

                let policy = caller.data().network_policy.clone();
                let max_response = policy
                    .max_response_bytes()
                    .min(caller.data().work_limits.http_response_bytes);
                // Reserve request storage plus the largest accepted completion
                // BEFORE spawning. Fast workers cannot accumulate uncharged
                // responses, and a successful request cannot lose its inbox slot.
                let reserved_bytes = max_response
                    .checked_add(std::mem::size_of::<crate::stage_access::HttpCompletion>())
                    .and_then(|n| n.checked_add(url.capacity()))
                    .and_then(|n| n.checked_add(content_type.capacity()))
                    .and_then(|n| n.checked_add(body.capacity()));
                let Some(reserved_bytes) = reserved_bytes else {
                    return ErrorCode::InvalidArgument.as_u32();
                };
                let Ok(reservation) = caller.data().queue_budget.reserve(reserved_bytes) else {
                    return ErrorCode::InvalidArgument.as_u32();
                };
                let inbox = std::sync::Arc::downgrade(&caller.data().http_inbox);
                outstanding.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let worker_outstanding = outstanding.clone();

                // Reserve a process-global worker slot in addition to the
                // per-instance quota, so N behaviors cannot collectively spawn
                // 8×N threads. `fetch_add` then roll back if we blew the ceiling:
                // under contention this transiently over-counts and so only ever
                // *rejects* a few extra requests, never admits past the ceiling.
                // The per-instance reservation above is undone on the same path.
                let prev_global =
                    GLOBAL_HTTP_WORKERS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if prev_global >= MAX_GLOBAL_HTTP_WORKERS {
                    GLOBAL_HTTP_WORKERS.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                    outstanding.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                    tracing::warn!(
                        request_id,
                        max = MAX_GLOBAL_HTTP_WORKERS,
                        "behavior net: process-global async worker ceiling reached; rejecting"
                    );
                    return ErrorCode::InvalidArgument.as_u32();
                }

                // Detached worker: owns its inputs + an Arc to the inbox, so it is
                // harmless if the instance detaches before it finishes (the push
                // lands in a queue nobody drains and is dropped with it). The
                // guard decrements both the per-instance and the process-global
                // counter even if the request panics.
                //
                // Use `Builder::spawn` + handle the error rather than the panicking
                // `std::thread::spawn`: an OS thread-limit failure must fail *this*
                // request, not unwind out of the wasm host call and abort the whole
                // runtime (mirrors exec.rs's poller spawns). On failure, undo both
                // in-flight reservations we just made so neither counter leaks.
                let spawned = std::thread::Builder::new()
                    .name("behavior-net".into())
                    .spawn(move || {
                        struct Decrement(std::sync::Arc<std::sync::atomic::AtomicUsize>);
                        impl Drop for Decrement {
                            fn drop(&mut self) {
                                self.0.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                                GLOBAL_HTTP_WORKERS
                                    .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                            }
                        }
                        let _guard = Decrement(worker_outstanding);

                        let (mut status, mut body) = match run_http_request(
                            method,
                            &url,
                            &content_type,
                            body,
                            &policy,
                            max_response,
                        ) {
                            HttpOutcome::Ok { status, body } => (status, body),
                            HttpOutcome::OverCap | HttpOutcome::Failed => (0, Vec::new()),
                        };
                        body.shrink_to_fit();
                        if body.capacity() > max_response {
                            status = 0;
                            body = Vec::new();
                        }
                        if let Some(inbox) = inbox.upgrade()
                            && let Ok(mut q) = inbox.lock()
                        {
                            let _ = q.push_reserved(
                                crate::stage_access::HttpCompletion {
                                    request_id,
                                    status,
                                    body,
                                },
                                reservation,
                            );
                        }
                    });
                if let Err(e) = spawned {
                    // The worker never started, so its drop guard will not run;
                    // release both reservations here.
                    outstanding.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                    GLOBAL_HTTP_WORKERS.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                    tracing::warn!(
                        request_id,
                        error = %e,
                        "behavior net: async worker spawn failed; rejecting"
                    );
                    return ErrorCode::InvalidArgument.as_u32();
                }
                ErrorCode::Ok.as_u32()
            },
        )?;

        // Read the body of the HTTP completion currently being delivered to
        // `_idtx_on_http_response` (seeded by the dispatcher for the hook's
        // duration; the status is passed to the hook directly). Copies ≤ `cap`
        // bytes and returns the body's *true* length (the caller-buffer +
        // true-length convention of `idtx_event_current_payload`).
        linker.func_wrap(
            "idtx",
            "idtx_http_response_body",
            |mut caller: Caller<'_, HostState>, out_ptr: u32, cap: u32| -> u32 {
                let body = caller.data().http_body().to_vec();
                let n = (u32::try_from(body.len()).unwrap_or(u32::MAX)).min(cap) as usize;
                write_bytes(&mut caller, out_ptr, &body[..n]);
                u32::try_from(body.len()).unwrap_or(u32::MAX)
            },
        )?;
    }

    Ok(linker)
}

// ---------------------------------------------------------------------------
// Shared HTTP egress (feature `net`)
// ---------------------------------------------------------------------------

/// Max in-flight asynchronous HTTP requests per behavior instance. Bounds the
/// background worker threads a behavior can spawn (e.g. one that fires a request
/// every frame). The 30 s request timeout guarantees each in-flight request
/// drains this counter, so a well-behaved behavior rarely approaches the cap.
#[cfg(feature = "net")]
const MAX_OUTSTANDING_HTTP: usize = 8;

/// Process-global ceiling on concurrent async HTTP worker threads across *all*
/// behavior instances. The per-instance [`MAX_OUTSTANDING_HTTP`] quota bounds a
/// single behavior; this bounds the whole host, so an untrusted stage that
/// attaches many `net` behaviors cannot collectively spawn `8 × N` detached
/// threads and exhaust process resources. A request is rejected once either
/// ceiling is reached. Sized well above the per-instance quota so a handful of
/// legitimate `net` behaviors are unaffected.
#[cfg(feature = "net")]
const MAX_GLOBAL_HTTP_WORKERS: usize = 64;

/// Live count of async HTTP worker threads across every behavior instance in
/// this process, compared against [`MAX_GLOBAL_HTTP_WORKERS`]. Reserved before a
/// worker is spawned and released by the worker's drop guard (so a panic still
/// frees the slot).
#[cfg(feature = "net")]
static GLOBAL_HTTP_WORKERS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Result of running one HTTP request through the SSRF gate.
#[cfg(feature = "net")]
enum HttpOutcome {
    /// Completed; `body` is already capped to the policy limit.
    Ok { status: u16, body: Vec<u8> },
    /// Completed, but the body exceeded the policy cap and was dropped.
    OverCap,
    /// Denied by the policy, or a transport-level failure (DNS/connect/TLS/read).
    Failed,
}

/// Split the `auth_write` host-scoped ABI's comma-separated `hosts` argument into
/// trimmed, non-empty host strings. A comma cannot appear in a hostname, so it is
/// an unambiguous delimiter for a credential's declared sharing group.
fn split_host_list(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .map(str::to_owned)
        .collect()
}

fn auth_result(result: Result<(), idtx_auth::AuthError>) -> u32 {
    match result {
        Ok(()) => ErrorCode::Ok.as_u32(),
        Err(_) => ErrorCode::InvalidArgument.as_u32(),
    }
}

#[cfg(feature = "net")]
/// Blocking worker-only HTTP. Both policy and reserved host-response caps apply;
/// guest hooks never call this synchronously.
fn run_http_request(
    method: reqwest::Method,
    url: &str,
    content_type: &str,
    body: Vec<u8>,
    policy: &idtx_net_guard::NetworkPolicy,
    max_response: usize,
) -> HttpOutcome {
    use std::io::Read;
    let max_bytes = policy.max_response_bytes().min(max_response);
    let validated = match idtx_net_guard::validate_endpoint(url, policy) {
        Ok(v) => v,
        Err(rejection) => {
            let url = idtx_net_guard::redact_url(url);
            // Distinguish a policy refusal (allowlist / scheme / SSRF — fix the
            // project `[network]` config) from a host the policy permits but that
            // could not be resolved (DNS / connectivity — not an allowlist issue),
            // so the log never blames the allowlist for a network outage.
            match &rejection {
                idtx_net_guard::EndpointRejection::Policy(reason) => tracing::warn!(
                    %url,
                    reason,
                    "behavior net: request denied by network policy"
                ),
                idtx_net_guard::EndpointRejection::Unresolved(reason) => tracing::warn!(
                    %url,
                    reason,
                    "behavior net: endpoint is allowed by policy but could not be resolved \
                     (DNS/connectivity, not an allowlist denial)"
                ),
            }
            return HttpOutcome::Failed;
        }
    };
    let client = match idtx_net_guard::locked_client(&validated, std::time::Duration::from_secs(30))
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "behavior net: HTTP client init failed");
            return HttpOutcome::Failed;
        }
    };

    let started = std::time::Instant::now();
    let retryable = method == reqwest::Method::GET || method == reqwest::Method::HEAD;
    let retry_method = method.clone();
    let retry_body = retryable.then(|| body.clone());
    let mut req = client
        .request(method, validated.url())
        .timeout(std::time::Duration::from_secs(30));
    if !content_type.is_empty() {
        req = req.header(reqwest::header::CONTENT_TYPE, content_type);
    }
    // Resolve for this exact validated request origin at send time, using the
    // same contract as USD assets and OpenExec REST polling.
    let credential = idtx_auth::authorization_header_for(validated.url());
    if let Some(authorization) = &credential {
        req = req.header(reqwest::header::AUTHORIZATION, authorization);
    }
    if !body.is_empty() {
        req = req.body(body);
    }

    match req.send() {
        Ok(mut resp) => {
            if resp.status().as_u16() == 401
                && retryable
                && idtx_auth::refresh_after_unauthorized(
                    validated.url(),
                    credential.as_deref(),
                    std::time::Duration::from_secs(30).saturating_sub(started.elapsed()),
                )
            {
                let remaining =
                    std::time::Duration::from_secs(30).saturating_sub(started.elapsed());
                if remaining.is_zero() {
                    return HttpOutcome::Failed;
                }
                let mut retry = client
                    .request(retry_method, validated.url())
                    .timeout(remaining);
                if !content_type.is_empty() {
                    retry = retry.header(reqwest::header::CONTENT_TYPE, content_type);
                }
                if let Some(body) = retry_body {
                    retry = retry.body(body);
                }
                if let Some(header) = idtx_auth::authorization_header_for(validated.url()) {
                    retry = retry.header(reqwest::header::AUTHORIZATION, header);
                }
                match retry.send() {
                    Ok(response) => resp = response,
                    Err(_) => return HttpOutcome::Failed,
                }
            }
            let status = resp.status().as_u16();
            // Enforce the cap WHILE reading, not after: inspect the advertised
            // `Content-Length` first, then stream through `Read::take(max_bytes + 1)`
            // so an absent or lying length cannot make us buffer an unbounded body
            // into host memory. The one extra byte distinguishes a body exactly at
            // the cap (kept) from one over it (dropped). Mirrors the REST
            // datasource's `read_capped_body` so both `net` paths bound memory
            // identically.
            if let Some(len) = resp.content_length()
                && len > max_bytes as u64
            {
                tracing::warn!(
                    content_length = len,
                    max_bytes,
                    "behavior net: response exceeds size limit; dropping"
                );
                return HttpOutcome::OverCap;
            }
            let mut body = Vec::new();
            let mut limited = resp.take(max_bytes as u64 + 1);
            if let Err(e) = limited.read_to_end(&mut body) {
                tracing::warn!(error = ?e.kind(), "behavior net: response read failed");
                return HttpOutcome::Failed;
            }
            if body.len() > max_bytes {
                tracing::warn!(
                    len = body.len(),
                    max_bytes,
                    "behavior net: response exceeds size limit; dropping"
                );
                return HttpOutcome::OverCap;
            }
            HttpOutcome::Ok { status, body }
        }
        Err(e) => {
            // `reqwest::Error`'s `Display` can embed the original full URL
            // (userinfo or query credentials included), so logging `%e` would
            // defeat the separately redacted `url` field. Strip the URL from the
            // error first with `without_url()`.
            tracing::warn!(error = %e.without_url(), url = %idtx_net_guard::redact_url(validated.url()), "behavior net: HTTP request failed");
            HttpOutcome::Failed
        }
    }
}

// ---------------------------------------------------------------------------
// Guest-memory marshalling helpers
// ---------------------------------------------------------------------------
fn range_valid(caller: &mut Caller<'_, HostState>, ptr: u32, len: usize) -> bool {
    let Some(mem) = memory(caller) else {
        return false;
    };
    (ptr as usize)
        .checked_add(len)
        .is_some_and(|end| end <= mem.data_size(&caller))
}

fn write_checked(caller: &mut Caller<'_, HostState>, ptr: u32, bytes: &[u8]) -> bool {
    let Some(mem) = memory(caller) else {
        return false;
    };
    mem.write(caller, ptr as usize, bytes).is_ok()
}

/// No partial JSON copies: insufficient capacity is an error, not truncation.
fn write_json_result(
    caller: &mut Caller<'_, HostState>,
    ptr: u32,
    cap: u32,
    len_ptr: u32,
    json: &str,
) -> u32 {
    if json.len() > idtx_behavior_abi::MAX_BEHAVIOR_CONTEXT_BYTES
        || cap as usize > idtx_behavior_abi::MAX_BEHAVIOR_CONTEXT_BYTES
        || cap as usize > caller.data().work_limits.argument_bytes
        || json.len() > cap as usize
        || !range_valid(caller, ptr, cap as usize)
        || !range_valid(caller, len_ptr, 4)
    {
        return ErrorCode::InvalidArgument.as_u32();
    }
    let len = u32::try_from(json.len()).unwrap_or(u32::MAX);
    if write_checked(caller, ptr, json.as_bytes())
        && write_checked(caller, len_ptr, &len.to_le_bytes())
    {
        ErrorCode::Ok.as_u32()
    } else {
        ErrorCode::InvalidArgument.as_u32()
    }
}

/// Fetch the guest's exported `memory`.
fn memory(caller: &mut Caller<'_, HostState>) -> Option<wasmtime::Memory> {
    caller
        .get_export("memory")
        .and_then(wasmtime::Extern::into_memory)
}

fn read_bytes(caller: &mut Caller<'_, HostState>, ptr: u32, len: u32) -> Option<Vec<u8>> {
    if len as usize > caller.data().work_limits.argument_bytes {
        return None;
    }
    if len == 0 {
        return Some(Vec::new());
    }
    let mem = memory(caller)?;
    let data = mem.data(&caller);
    let start = ptr as usize;
    let end = start.checked_add(len as usize)?;
    data.get(start..end).map(<[u8]>::to_vec)
}

fn read_string(caller: &mut Caller<'_, HostState>, ptr: u32, len: u32) -> Option<String> {
    let bytes = read_bytes(caller, ptr, len)?;
    String::from_utf8(bytes).ok()
}

fn read_xform(caller: &mut Caller<'_, HostState>, ptr: u32) -> Option<Xform> {
    read_pod(caller, ptr)
}

/// Read a POD `T` from `ptr` (unaligned — guest buffers carry no alignment
/// guarantee). `None` when the guest memory range is out of bounds.
fn read_pod<T: bytemuck::Pod>(caller: &mut Caller<'_, HostState>, ptr: u32) -> Option<T> {
    // Fixed-size ABI values need no heap allocation or guest-selected length.
    // The variable-argument cap must not accidentally disable POD operations.
    let mem = memory(caller)?;
    let start = ptr as usize;
    let end = start.checked_add(std::mem::size_of::<T>())?;
    Some(bytemuck::pod_read_unaligned(
        mem.data(&caller).get(start..end)?,
    ))
}

/// Read a `glam::Vec3` (12 bytes) from `ptr`, defaulting to zero on a bad range.
fn read_vec3(caller: &mut Caller<'_, HostState>, ptr: u32) -> glam::Vec3 {
    read_pod::<glam::Vec3>(caller, ptr).unwrap_or(glam::Vec3::ZERO)
}

/// Read a `glam::Vec4` (16 bytes) from `ptr`, defaulting to zero on a bad range.
fn read_vec4(caller: &mut Caller<'_, HostState>, ptr: u32) -> glam::Vec4 {
    read_pod::<glam::Vec4>(caller, ptr).unwrap_or(glam::Vec4::ZERO)
}

/// Write ≤ `cap` bytes of `path` to `ptr` and return its *true* byte length so
/// the guest can detect truncation (the `idtx_event_current_name` convention).
/// `None`/empty path writes nothing and returns 0.
fn write_path(caller: &mut Caller<'_, HostState>, ptr: u32, cap: u32, path: Option<&str>) -> u32 {
    let Some(path) = path else {
        return 0;
    };
    let bytes = path.as_bytes();
    let n = (u32::try_from(bytes.len()).unwrap_or(u32::MAX)).min(cap) as usize;
    write_bytes(caller, ptr, &bytes[..n]);
    u32::try_from(bytes.len()).unwrap_or(u32::MAX)
}

fn write_bytes(caller: &mut Caller<'_, HostState>, ptr: u32, bytes: &[u8]) {
    let Some(mem) = memory(caller) else {
        return;
    };
    let _ = mem.write(caller, ptr as usize, bytes);
}

fn write_xform(caller: &mut Caller<'_, HostState>, ptr: u32, x: &Xform) {
    write_bytes(caller, ptr, bytemuck::bytes_of(x));
}

fn write_pose(caller: &mut Caller<'_, HostState>, ptr: u32, pose: &XrPoseResult) {
    write_bytes(caller, ptr, bytemuck::bytes_of(pose));
}

fn write_entity(caller: &mut Caller<'_, HostState>, ptr: u32, e: WireEntity) {
    write_bytes(caller, ptr, bytemuck::bytes_of(&e));
}

fn write_f32x2(caller: &mut Caller<'_, HostState>, ptr: u32, v: [f32; 2]) {
    let mut buf = [0u8; 8];
    buf[0..4].copy_from_slice(&v[0].to_le_bytes());
    buf[4..8].copy_from_slice(&v[1].to_le_bytes());
    write_bytes(caller, ptr, &buf);
}
