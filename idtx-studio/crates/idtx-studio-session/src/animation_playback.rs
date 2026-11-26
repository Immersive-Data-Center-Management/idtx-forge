//! Shared runtime/editor action controller integration.
use super::*;
use idtx_scene::playback::{self, ClipTransport};

#[cfg(all(test, feature = "authoring"))]
mod tests;

pub(super) fn operation(cmd: &PlaybackCommand) -> ClipTransport {
    match cmd.kind {
        PlaybackCommandKind::Play => ClipTransport::Play,
        PlaybackCommandKind::Pause => ClipTransport::Pause,
        PlaybackCommandKind::Stop => ClipTransport::Stop,
        PlaybackCommandKind::Seek => ClipTransport::Seek(f64::from(cmd.value)),
        PlaybackCommandKind::SetSpeed => ClipTransport::SetSpeed(f64::from(cmd.value)),
        PlaybackCommandKind::SetLooping => ClipTransport::SetLooping(cmd.value != 0.0),
        PlaybackCommandKind::Release => ClipTransport::Release,
    }
}

pub(super) fn apply_command(world: &mut idtx_scene::bevy_ecs::world::World, service: &StageService,
    cmd: &PlaybackCommand) -> Result<(), String>
{
    let result = apply_command_inner(world, service, cmd);
    record_outcome(world, cmd, 0, &result);
    result
}

pub(super) fn record_outcome(world: &mut idtx_scene::bevy_ecs::world::World, cmd: &PlaybackCommand,
    sequence: u64, result: &Result<(), String>) {
    if let Some(entity) = idtx_scene::bevy_ecs::entity::Entity::try_from_bits(cmd.target.to_bits()) {
        if let Some(mut player) = world.get_mut::<AnimClipPlayer>(entity) {
            if let Some(head) = player.playhead_mut(&cmd.clip) {
                head.last_command_sequence = sequence;
                head.last_command_applied = Some(result.is_ok());
                head.last_command_error = result.as_ref().err().cloned().unwrap_or_default();
            }
        }
    }
}

fn apply_command_inner(world: &mut idtx_scene::bevy_ecs::world::World, service: &StageService,
    cmd: &PlaybackCommand) -> Result<(), String> {
    let entity = idtx_scene::bevy_ecs::entity::Entity::try_from_bits(cmd.target.to_bits())
        .ok_or("invalid animation target")?;
    let candidate = world.get::<AnimClipPlayer>(entity).and_then(|p| p.playhead(&cmd.clip))
        .ok_or_else(|| format!("missing animation player/clip {} on {:?}", cmd.clip, cmd.target))?;
    if matches!(cmd.kind, PlaybackCommandKind::Play | PlaybackCommandKind::Pause | PlaybackCommandKind::Seek) {
        if let Some(declaration) = &candidate.declaration {
            service.prepare_animation_clip(declaration).map_err(|e| e.to_string())?;
        }
    }
    playback::transport(world, entity, &cmd.clip, operation(cmd))
}

pub(super) fn status_json(world: &idtx_scene::bevy_ecs::world::World,
    entity: idtx_scene::bevy_ecs::entity::Entity, label: &str) -> Option<Vec<u8>>
{
    let head = world.get::<AnimClipPlayer>(entity)?.playhead(label)?;
    let declaration = head.declaration.as_ref();
    let state = if !head.error.is_empty() { "invalid" } else if !head.independent { "following" }
        else if head.playing { "playing" } else if head.completed { "completed" }
        else if head.stopped { "stopped" } else { "paused" };
    let finite = |v: f64| if v.is_finite() { v } else { 0.0 };
    serde_json::to_vec(&serde_json::json!({
        "owner": world.get::<PrimNode>(entity).map_or("", |n| n.path.as_str()),
        "clip": head.clip, "channel": declaration.map_or("body", |d| d.channel.as_str()),
        "source": effective_source(world, head), "state": state,
        "error": head.error, "current": finite(head.current_time_code),
        "start": finite(head.start_time_code), "end": finite(head.end_time_code),
        "time_codes_per_second": finite(head.time_codes_per_second),
        "speed": finite(head.speed), "looping": head.looping, "independent": head.independent,
        "run_id": head.run_id,
        "last_command_sequence": head.last_command_sequence,
        "last_command_applied": head.last_command_applied,
        "last_command_error": head.last_command_error,
    })).ok()
}

fn effective_source(world: &idtx_scene::bevy_ecs::world::World, head: &ClipPlayhead) -> String {
    let roots: Vec<_> = world.iter_entities().filter_map(|e| e.get::<PrimNode>()
        .filter(|n| head.targets.contains(&n.prim)).map(|n| n.path.as_str())).collect();
    let source = world.iter_entities().find_map(|e| {
        let node = e.get::<PrimNode>()?;
        if !roots.iter().any(|r| playback::within(&node.path, r)) { return None; }
        e.get::<idtx_scene::SkeletonPoseComp>()?.record.animation_source
    });
    source.and_then(|source| world.iter_entities().find_map(|e| e.get::<PrimNode>()
        .filter(|n| n.prim == source).map(|n| n.path.clone()))).unwrap_or_default()
}

impl StudioSession {
    pub fn clip_effective_source(&self, owner: &str, clip: &str) -> String {
        self.clip_player_state(owner).iter().find(|h| h.clip == clip)
            .map(|h| effective_source(self.hydrator.world(), h)).unwrap_or_default()
    }
    pub fn control_clip(&mut self, owner: &str, clip: &str, op: ClipTransport) -> Result<(), String> {
        let (kind, value) = match op {
            ClipTransport::Play => (PlaybackCommandKind::Play, 0.0),
            ClipTransport::Pause => (PlaybackCommandKind::Pause, 0.0),
            ClipTransport::Stop => (PlaybackCommandKind::Stop, 0.0),
            ClipTransport::Release => (PlaybackCommandKind::Release, 0.0),
            ClipTransport::Seek(v) => (PlaybackCommandKind::Seek, v as f32),
            ClipTransport::SetSpeed(v) => (PlaybackCommandKind::SetSpeed, v as f32),
            ClipTransport::SetLooping(v) => (PlaybackCommandKind::SetLooping, if v { 1.0 } else { 0.0 }),
        };
        if !value.is_finite() { return Err("transport value exceeds finite f32 range".into()); }
        self.clip_command(owner, clip, kind, value)
    }

    #[cfg(feature = "authoring")]
    pub fn author_animation_clip(&mut self, clip: &idtx_usd::AnimationClip, remove: bool) -> Result<(), SessionError> {
        use idtx_usd::{PropertyValue as V, StructuralCommand};
        if clip.name.is_empty() || clip.name.len() > 256 || !clip.name.chars().enumerate().all(|(i, c)| c == '_' || c.is_ascii_alphabetic() || (i > 0 && c.is_ascii_digit()))
            || (!remove && (!clip.start.is_finite() || !clip.end.is_finite() || clip.end <= clip.start || !clip.speed.is_finite())) {
            return Err(StageError::OpenUsd("clip requires an identifier, finite speed and nonempty range".into()).into());
        }
        let prim = self.service.prim_at_path(&clip.owner).ok_or_else(|| StageError::UnknownPath(clip.owner.clone()))?;
        let mut names: Vec<_> = self.service.animation_clips(&clip.owner)?.into_iter().map(|c| c.name).filter(|n| !n.is_empty()).collect();
        names.retain(|n| n != &clip.name);
        let before = self.root_layer_snapshot()?;
        let result = (|| -> Result<(), SessionError> {
            if !remove {
                names.push(clip.name.clone());
                let prefix = format!("idtx:animation:{}:", clip.name);
                for (property, value) in [("start", V::TimeCode(clip.start)), ("end", V::TimeCode(clip.end)),
                    ("speed", V::F64(clip.speed)), ("loop", V::Bool(clip.looping)),
                    ("autoPlay", V::Bool(clip.autoplay)), ("channel", V::Token(clip.channel.clone()))] {
                    self.service.apply_structural_command(&StructuralCommand::SetProperty {
                        prim, property: format!("{prefix}{property}"), value,
                    })?;
                }
                self.service.set_relationship_targets(&clip.owner, &format!("{prefix}targets"), &clip.targets)?;
                self.service.set_relationship_targets(&clip.owner, &format!("{prefix}source"),
                    &if clip.source.is_empty() { Vec::new() } else { vec![clip.source.clone()] })?;
            }
            for (property, value) in [(idtx_usd::ANIMATION_VERSION, V::Int(1)), (idtx_usd::ANIMATION_CLIP_NAMES, V::TokenArray(names))] {
                self.service.apply_structural_command(&StructuralCommand::SetProperty { prim, property: property.to_owned(), value })?;
            }
            self.tick()?;
            playback::reconcile(self.hydrator.world_mut(), &self.service)?;
            Ok(())
        })();
        if result.is_err() {
            let after = self.root_layer_snapshot()?;
            self.apply_root_layer_delta(&after, &before)?;
        }
        result
    }

    /// Same addressed controller used by behavior commands and Conductor preview.
    pub fn clip_command(&mut self, owner: &str, clip: &str, kind: PlaybackCommandKind, value: f32) -> Result<(), String> {
        playback::reconcile(self.hydrator.world_mut(), &self.service).map_err(|e| e.to_string())?;
        let entity = self.hydrator.world().iter_entities().find(|e| e.get::<PrimNode>().is_some_and(|n| n.path == owner))
            .map(|e| e.id()).ok_or_else(|| format!("missing playback owner {owner}"))?;
        let command = PlaybackCommand { target: idtx_behavior::entity_to_wire(entity), clip: clip.to_owned(), kind, value };
        apply_command(self.hydrator.world_mut(), &self.service, &command)?;
        if kind == PlaybackCommandKind::Release {
            let head = self.hydrator.world().get::<AnimClipPlayer>(entity).and_then(|p| p.playhead(clip)).cloned();
            if let Some(head) = head {
                if let Some(d) = &head.declaration { self.service.release_animation_clip(&d.key()).map_err(|e| e.to_string())?; }
                let tc = self.animation_state().1.map_or(0.0, |c| c.current_time_code);
                if !head.targets.is_empty() {
                    let mut projection = self.service.sample_viewport_roots(tc, &head.targets).map_err(|e| e.to_string())?;
                    let owned = idtx_scene::collect_independent_clip_target_prims(self.hydrator.world_mut());
                    projection.exclude_prims(&owned);
                    self.hydrator.apply_viewport_projection(&projection).map_err(|e| e.to_string())?;
                    self.snapshot_dirty = true;
                }
            }
        }
        Ok(())
    }

    pub fn clip_player_state(&self, owner: &str) -> Vec<ClipPlayhead> {
        self.hydrator.world().iter_entities().find(|e| e.get::<PrimNode>().is_some_and(|n| n.path == owner))
            .and_then(|e| e.get::<AnimClipPlayer>()).map_or_else(Vec::new, |p| p.playheads.clone())
    }

    pub(super) fn tick_authored_clip_animation(&mut self, dt: f64) -> Result<(), SessionError> {
        playback::reconcile(self.hydrator.world_mut(), &self.service)?;
        let restore = self.hydrator.world_mut().get_resource_mut::<playback::ClipRestoreTargets>()
            .map(|mut r| std::mem::take(&mut r.0)).unwrap_or_default();
        let mut restore: Vec<_> = restore.into_iter().filter(|id| self.service.path_for(*id)
            .is_some_and(|p| self.service.prim_at_path(p) == Some(*id))).collect();
        let mut seen = std::collections::HashSet::new();
        restore.retain(|id| seen.insert(*id));
        if !restore.is_empty() {
            let tc = self.animation_state().1.map_or(0.0, |c| c.current_time_code);
            let mut projection = self.service.sample_viewport_roots(tc, &restore)?;
            projection.exclude_prims(&idtx_scene::collect_independent_clip_target_prims(self.hydrator.world_mut()));
            self.hydrator.apply_animation_projection(&projection)?;
            self.snapshot_dirty = true;
        }
        let playing = self.play_state == PlayState::Playing;
        let mut autoplay = Vec::new();
        {
            let world = self.hydrator.world_mut();
            let mut q = world.query::<(idtx_scene::bevy_ecs::entity::Entity, &mut AnimClipPlayer)>();
            for (entity, mut player) in q.iter_mut(world) {
                for head in &mut player.playheads {
                    if playing && !head.autoplay_started && head.error.is_empty()
                        && head.declaration.as_ref().is_some_and(|d| d.autoplay) {
                        head.autoplay_started = true;
                        autoplay.push(PlaybackCommand { target: idtx_behavior::entity_to_wire(entity), clip: head.clip.clone(),
                            kind: PlaybackCommandKind::Play, value: 0.0 });
                    }
                }
            }
        }
        for command in autoplay {
            if let Err(error) = apply_command(self.hydrator.world_mut(), &self.service, &command) {
                tracing::warn!(%error, "animation autoplay failed");
            }
        }
        for entity in self.hydrator.world().iter_entities() {
            if let Some(player) = entity.get::<AnimClipPlayer>() {
                for head in &player.playheads {
                    if !head.independent {
                        if let Some(d) = &head.declaration { self.service.release_animation_clip(&d.key())?; }
                    }
                }
            }
        }
        idtx_scene::tick_clip_players(self.hydrator.world_mut(), dt);
        let placement_tc = self.animation_state().1.map_or(0.0, |c| c.current_time_code);
        let animated_parents: Vec<_> = self.hydrator.world().iter_entities().filter_map(|e|
            e.get::<idtx_scene::AnimatedAttribute>().and_then(|_| e.get::<PrimNode>()).map(|n| n.path.clone())).collect();
        let requests: Vec<_> = self.hydrator.world().iter_entities().flat_map(|e| {
            e.get::<AnimClipPlayer>().map(|p| p.playheads.iter().filter(|h| h.independent && h.error.is_empty()
                && h.has_range() && !h.targets.is_empty() && (h.sample_pending() ||
                    (h.last_placement_time_code != placement_tc && h.declaration.as_ref().is_some_and(|d|
                        d.targets.iter().any(|root| animated_parents.iter().any(|parent| root != parent && playback::within(root, parent)))))))
                .map(|h| (e.id(), h.clone())).collect::<Vec<_>>()).unwrap_or_default()
        }).collect();
        for (entity, head) in requests {
            let transforms_only = !head.needs_initial_sample && head.declaration.as_ref().is_some_and(|d| d.source.is_empty())
                && self.hydrator.world().iter_entities().all(|e| {
                    let inside = e.get::<PrimNode>().is_some_and(|n| head.declaration.as_ref().is_some_and(|d|
                        d.targets.iter().any(|r| playback::within(&n.path, r))));
                    !inside || (e.get::<idtx_scene::SkeletonPoseComp>().is_none() && e.get::<idtx_scene::CurveComp>().is_none()
                        && e.get::<idtx_scene::AnimatedAttribute>().is_none_or(|a| a.tracks.iter().all(|t|
                            matches!(&t.target, idtx_scene::TrackTarget::Attribute { attr_name } if attr_name.starts_with("xformOp:")))))
                });
            let poses_only = !head.needs_initial_sample && head.declaration.as_ref().is_some_and(|d| !d.source.is_empty())
                && self.hydrator.world().iter_entities().all(|e| {
                    let inside = e.get::<PrimNode>().is_some_and(|n| head.declaration.as_ref().is_some_and(|d|
                        d.targets.iter().any(|r| playback::within(&n.path, r))));
                    !inside || (e.get::<idtx_scene::SkinBindingComp>().is_none_or(|s| s.record.execution == idtx_usd::SkinningExecution::GpuClassicLinear)
                        && e.get::<idtx_scene::AnimatedAttribute>().is_none_or(|a| a.tracks.iter().all(|t|
                            matches!(&t.target, idtx_scene::TrackTarget::Attribute { attr_name } if matches!(attr_name.as_str(), "rotations" | "translations" | "scales" | "extent")))))
                });
            let sample = if let Some(d) = &head.declaration {
                if transforms_only { self.service.sample_animation_clip_transforms(d, head.current_time_code, placement_tc) }
                else { self.service.sample_animation_clip_with_placement(d, head.current_time_code, placement_tc, poses_only) }
            } else { self.service.sample_viewport_roots(head.current_time_code, &head.targets) };
            match sample {
                Ok(projection) => {
                    for diagnostic in &projection.diagnostics {
                        if !self.diagnostics.contains(diagnostic) {
                            tracing::warn!(clip = %head.clip, %diagnostic, "native animation diagnostic");
                            self.diagnostics.push(diagnostic.clone());
                        }
                    }
                    if transforms_only {
                        let patches = self.hydrator.apply_transform_samples(&projection.transforms)?;
                        self.snapshot_dirty |= self.hydrator.apply_animation_visibility(&projection)?;
                        self.merge_transform_patches(patches);
                        if self.last_snapshot.is_none() { self.snapshot_dirty = true; }
                    } else if poses_only {
                        self.hydrator.apply_skeleton_poses(&projection)?;
                        self.snapshot_dirty |= self.hydrator.apply_animation_visibility(&projection)?;
                        if self.last_snapshot.is_some() { self.snapshot_skeleton_dirty = true; } else { self.snapshot_dirty = true; }
                    } else {
                        self.hydrator.apply_animation_projection(&projection)?;
                        self.snapshot_dirty = true;
                        self.snapshot_transform_patches = None;
                    }
                    if let Some(mut player) = self.hydrator.world_mut().get_mut::<AnimClipPlayer>(entity) {
                        if let Some(current) = player.playhead_mut(&head.clip) { current.mark_sampled(); current.last_placement_time_code = placement_tc; }
                    }
                }
                Err(error) => {
                    tracing::warn!(clip = %head.clip, %error, "animation sample failed");
                    if let Some(d) = &head.declaration { self.service.release_animation_clip(&d.key())?; }
                    let mut event = playback::ClipEvent::new(entity, &head, "animation.failed");
                    event.detail = error.to_string();
                    if let Some(mut player) = self.hydrator.world_mut().get_mut::<AnimClipPlayer>(entity) {
                        if let Some(current) = player.playhead_mut(&head.clip) { current.error = error.to_string(); current.release(); }
                    }
                    let world = self.hydrator.world_mut();
                    world.init_resource::<playback::ClipRestoreTargets>();
                    world.resource_mut::<playback::ClipRestoreTargets>().0.extend(head.targets);
                    playback::queue_event(world, event);
                }
            }
        }
        if let Some(mut events) = self.hydrator.world_mut().get_resource_mut::<playback::ClipEvents>() {
            let pending = std::mem::take(&mut events.0);
            for event in pending {
                let payload = serde_json::to_vec(&serde_json::json!({"owner":event.owner.to_bits(), "clip":event.clip, "channel":event.channel,
                    "run_id":event.run_id, "error":event.detail})).unwrap_or_default();
                if let Err(error) = self.behaviors.post_playback_event(idtx_behavior::entity_to_wire(event.owner), None, event.name, &payload) {
                    tracing::debug!(?error, "playback event queue full; status remains queryable");
                }
            }
        }
        Ok(())
    }

    pub(super) fn reset_animation_players(&mut self) {
        let mut restore = Vec::new();
        let world = self.hydrator.world_mut();
        let mut q = world.query::<&mut AnimClipPlayer>();
        for mut player in q.iter_mut(world) {
            for head in &mut player.playheads {
                if head.independent { restore.extend_from_slice(&head.targets); }
                head.release(); head.stop(); head.autoplay_started = false;
                if let Some(d) = &head.declaration { let _ = self.service.release_animation_clip(&d.key()); }
            }
        }
        world.init_resource::<playback::ClipRestoreTargets>();
        world.resource_mut::<playback::ClipRestoreTargets>().0.extend(restore);
    }
}
