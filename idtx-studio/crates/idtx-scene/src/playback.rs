//! Authored action reconciliation, exclusive ownership and observable transport.
use bevy_ecs::prelude::*;
use idtx_usd::{StageRevision, StageService};
use std::collections::{HashSet, VecDeque};
use crate::{AnimClipPlayer, ClipPlayhead, PrimNode, StagePlayback};

pub fn within(path: &str, root: &str) -> bool {
    root == "/" || path == root || path.strip_prefix(root).is_some_and(|s| s.starts_with('/'))
}

#[derive(Debug, Clone)]
pub struct ClipEvent {
    pub owner: Entity,
    pub clip: String,
    pub channel: String,
    pub run_id: u64,
    pub name: &'static str,
    pub detail: String,
}
impl ClipEvent {
    pub fn new(owner: Entity, head: &ClipPlayhead, name: &'static str) -> Self {
        Self { owner, clip: head.clip.clone(), channel: head.declaration.as_ref()
            .map_or_else(|| "body".to_owned(), |d| d.channel.clone()), run_id: head.run_id,
            name, detail: head.error.clone() }
    }
}

#[derive(Resource, Default, Debug)]
pub struct ClipEvents(pub VecDeque<ClipEvent>);

#[derive(Resource, Default)]
pub struct ClipRestoreTargets(pub Vec<idtx_usd::PrimId>);

/// Release bookkeeping shared by interruption, explicit release and teardown.
/// Sampling excludes targets acquired by another head before this queue drains.
pub fn release_head(head: &mut ClipPlayhead, restore: &mut Vec<idtx_usd::PrimId>) {
    if head.independent { restore.extend_from_slice(&head.targets); }
    head.release();
}

pub fn queue_restore(world: &mut World, targets: Vec<idtx_usd::PrimId>) {
    if targets.is_empty() { return; }
    world.init_resource::<ClipRestoreTargets>();
    world.resource_mut::<ClipRestoreTargets>().0.extend(targets);
    let mut clocks = world.query::<&mut StagePlayback>();
    for mut clock in clocks.iter_mut(world) { clock.needs_initial_sample = true; }
}

pub fn release_player(world: &mut World, entity: Entity) {
    let mut restore = Vec::new();
    if let Some(mut player) = world.get_mut::<AnimClipPlayer>(entity) {
        for head in &mut player.playheads { release_head(head, &mut restore); }
    }
    queue_restore(world, restore);
}

pub fn queue_event(world: &mut World, event: ClipEvent) {
    world.init_resource::<ClipEvents>();
    let mut events = world.resource_mut::<ClipEvents>();
    // Durable head state is still queryable if the consumer falls behind.
    if events.0.len() < 4096 { events.0.push_back(event); }
}

#[derive(Resource, Default)]
pub struct PlaybackReconcile {
    revision: Option<StageRevision>,
    pub dirty_paths: Vec<String>,
    keys: HashSet<String>,
}

/// Called before animation ticks/behavior attachment. No stage query on quiet
/// frames; unchanged declarations retain transport overrides and run identity.
pub fn reconcile(world: &mut World, service: &StageService) -> Result<(), idtx_usd::StageError> {
    world.init_resource::<PlaybackReconcile>();
    if world.resource::<PlaybackReconcile>().revision == Some(service.revision())
        && world.resource::<PlaybackReconcile>().dirty_paths.is_empty() { return Ok(()); }
    let initial = world.resource::<PlaybackReconcile>().revision.is_none();
    let dirty = std::mem::take(&mut world.resource_mut::<PlaybackReconcile>().dirty_paths);
    let owners: Vec<_> = world.iter_entities().filter_map(|e| e.get::<PrimNode>()
        .filter(|n| !n.is_pseudo_root).map(|n| (e.id(), n.path.clone()))).collect();
    let mut keys = HashSet::new();
    let mut restore = Vec::new();
    let mut events = Vec::new();
    let mut revalidate = Vec::new();
    for (entity, path) in owners {
        let existing: Vec<_> = world.get::<AnimClipPlayer>(entity).map(|p| p.playheads.iter()
            .filter_map(|h| h.declaration.clone()).collect()).unwrap_or_default();
        let affected_owner = initial || dirty.iter().any(|p| within(p, &path) || within(&path, p)
            || existing.iter().any(|d| !d.source.is_empty() && (within(p, &d.source) || within(&d.source, p))));
        if !affected_owner {
            keys.extend(existing.iter().map(|d| d.key()));
            continue;
        }
        let declarations = service.animation_clips(&path)?;
        if declarations.is_empty() && world.get::<AnimClipPlayer>(entity).is_none() { continue; }
        if world.get::<AnimClipPlayer>(entity).is_none() { world.entity_mut(entity).insert(AnimClipPlayer::new()); }
        let mut heads = world.get_mut::<AnimClipPlayer>(entity).expect("player");
        let names: HashSet<_> = declarations.iter().map(|d| d.name.clone()).collect();
        heads.playheads.retain(|h| {
            let keep = h.declaration.is_none() || names.contains(&h.clip);
            if !keep && h.independent {
                restore.extend_from_slice(&h.targets);
                let mut event = ClipEvent::new(entity, h, "animation.interrupted");
                event.detail = "clip declaration removed".into();
                events.push(event);
            }
            keep
        });
        for declaration in declarations {
            keys.insert(declaration.key());
            let targets = declaration.targets.iter().filter_map(|p| service.prim_at_path(p)).collect::<Vec<_>>();
            let old = heads.playhead(&declaration.name);
            let changed = old.is_none_or(|h| h.declaration.as_ref() != Some(&declaration) || h.targets != targets
                || h.time_codes_per_second != service.animation().time_codes_per_second);
            let affected = dirty.iter().any(|p| within(p, &path) || within(&path, p)
                || (!declaration.source.is_empty() && (within(p, &declaration.source) || within(&declaration.source, p))));
            let mut binding_error = declaration.error.clone();
            if changed || affected {
                if let Err(error) = service.prepare_animation_clip(&declaration) {
                    if binding_error.is_empty() { binding_error = error.to_string(); }
                    service.release_animation_clip(&declaration.key())?;
                }
            }
            if let Some(head) = heads.playhead_mut(&declaration.name) {
                if changed || affected {
                    let previous = head.declaration.as_ref();
                    if head.independent && (head.targets != targets || previous.is_some_and(|d| d.channel != declaration.channel)) {
                        restore.extend_from_slice(&head.targets);
                        revalidate.push((entity, head.clip.clone()));
                    }
                    if previous.is_none_or(|d| d.speed != declaration.speed) { head.speed = declaration.speed; }
                    if previous.is_none_or(|d| d.looping != declaration.looping) { head.looping = declaration.looping; }
                    head.start_time_code = declaration.start;
                    head.end_time_code = declaration.end;
                    if declaration.end > declaration.start && declaration.start.is_finite() && declaration.end.is_finite() {
                        head.current_time_code = head.current_time_code.clamp(declaration.start, declaration.end);
                    }
                    if head.independent && !binding_error.is_empty() { restore.extend_from_slice(&head.targets); }
                    head.targets = targets;
                    head.time_codes_per_second = service.animation().time_codes_per_second;
                    head.needs_initial_sample = true;
                    head.error = binding_error;
                    if !head.error.is_empty() {
                        if head.independent { restore.extend_from_slice(&head.targets); }
                        head.release();
                        tracing::warn!(owner = %path, clip = %head.clip, error = %head.error, "invalid animation binding");
                        events.push(ClipEvent::new(entity, head, "animation.failed"));
                    }
                    head.declaration = Some(declaration);
                }
            } else {
                let mut head = ClipPlayhead::new(&declaration.name, declaration.start, declaration.end,
                    service.animation().time_codes_per_second).with_targets(targets).following();
                head.speed = declaration.speed;
                head.looping = declaration.looping;
                head.error = binding_error;
                head.declaration = Some(declaration);
                if !head.error.is_empty() {
                    tracing::warn!(owner = %path, clip = %head.clip, error = %head.error, "invalid animation declaration");
                    events.push(ClipEvent::new(entity, &head, "animation.failed"));
                }
                heads.set_playhead(head);
            }
        }
    }
    // An authored target/channel edit must obey the same ownership contract as
    // Play. Suspend conflicting edited heads; never let iteration order choose
    // which clock overwrites another actor.
    let paths: std::collections::HashMap<_, _> = world.iter_entities().filter_map(|e|
        e.get::<PrimNode>().map(|n| (n.prim, n.path.clone()))).collect();
    let active: Vec<_> = world.iter_entities().flat_map(|e| {
        e.get::<AnimClipPlayer>().map(|p| p.playheads.iter().filter(|h| h.independent)
            .map(|h| (e.id(), h.clip.clone(), h.declaration.as_ref().map_or("body", |d| d.channel.as_str()).to_owned(),
                h.targets.iter().filter_map(|id| paths.get(id).cloned()).collect::<Vec<_>>())).collect::<Vec<_>>()).unwrap_or_default()
    }).collect();
    for (owner, label) in revalidate {
        let Some(candidate) = active.iter().find(|a| a.0 == owner && a.1 == label) else { continue };
        if let Some(conflict) = active.iter().find(|a| (a.0 != owner || a.1 != label) &&
            ((a.0 == owner && a.2 == candidate.2) || a.3.iter().any(|p| candidate.3.iter().any(|t| within(p, t) || within(t, p))))) {
            if let Some(mut player) = world.get_mut::<AnimClipPlayer>(owner) {
                if let Some(head) = player.playhead_mut(&label) {
                    restore.extend_from_slice(&head.targets);
                    head.error = format!("authored animation ownership conflicts with {}", conflict.1);
                    head.release();
                    if let Some(d) = &head.declaration { service.release_animation_clip(&d.key())?; }
                    events.push(ClipEvent::new(owner, head, "animation.failed"));
                }
            }
        }
    }
    for key in world.resource::<PlaybackReconcile>().keys.difference(&keys) {
        service.release_animation_clip(key)?;
    }
    let mut state = world.resource_mut::<PlaybackReconcile>();
    state.keys = keys;
    state.revision = Some(service.revision());
    drop(state);
    queue_restore(world, restore);
    for event in events { queue_event(world, event); }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
pub enum ClipTransport { Play, Pause, Stop, Seek(f64), SetSpeed(f64), SetLooping(bool), Release }

pub fn transport(world: &mut World, owner: Entity, label: &str, op: ClipTransport) -> Result<(), String> {
    let candidate = world.get::<AnimClipPlayer>(owner).ok_or("target has no animation player")?
        .playhead(label).ok_or_else(|| format!("unknown animation clip {label}"))?.clone();
    if !candidate.error.is_empty() && !matches!(op, ClipTransport::Release | ClipTransport::Stop) {
        return Err(candidate.error);
    }
    if let ClipTransport::Seek(value) | ClipTransport::SetSpeed(value) = op {
        if !value.is_finite() { return Err("animation value must be finite".into()); }
    }
    let acquire = matches!(op, ClipTransport::Play | ClipTransport::Pause | ClipTransport::Seek(_));
    if acquire && candidate.discovered && candidate.targets.is_empty() {
        return Err("discovered animation source has no bound consumers; declare an actor clip to select it".into());
    }
    let paths = |ids: &[idtx_usd::PrimId]| -> Vec<String> {
        world.iter_entities().filter_map(|e| e.get::<PrimNode>()
            .filter(|n| ids.contains(&n.prim)).map(|n| n.path.clone())).collect()
    };
    let targets = paths(&candidate.targets);
    let channel = candidate.declaration.as_ref().map_or("body", |d| d.channel.as_str());
    let mut interrupted = Vec::new();
    if acquire {
        for e in world.iter_entities() {
            let Some(player) = e.get::<AnimClipPlayer>() else { continue };
            for head in &player.playheads {
                if !head.independent || (e.id() == owner && head.clip == label) { continue; }
                let other_channel = head.declaration.as_ref().map_or("body", |d| d.channel.as_str());
                if e.id() == owner && other_channel == channel {
                    interrupted.push(head.clip.clone());
                } else if paths(&head.targets).iter().any(|p| targets.iter().any(|t| within(p, t) || within(t, p))) {
                    return Err(format!("animation ownership conflict with {}", head.clip));
                }
            }
        }
    }
    let scope_time = world.iter_entities().find_map(|e| e.get::<StagePlayback>().map(|c| c.current_time_code));
    let mut events = Vec::new();
    let mut restore = Vec::new();
    let mut player = world.get_mut::<AnimClipPlayer>(owner).expect("validated player");
    for label in interrupted {
        let head = player.playhead_mut(&label).expect("candidate");
        release_head(head, &mut restore);
        events.push(ClipEvent::new(owner, head, "animation.interrupted"));
    }
    let head = player.playhead_mut(label).expect("validated clip");
    if acquire {
        if matches!(op, ClipTransport::Pause) && !head.independent {
            if let Some(tc) = scope_time { head.scrub_to(tc); }
        }
        head.acquire();
    }
    match op {
        ClipTransport::Play => {
            let finished = !head.looping && !head.playing && ((head.speed >= 0.0 && head.current_time_code >= head.end_time_code)
                || (head.speed < 0.0 && head.current_time_code <= head.start_time_code));
            if head.completed || finished || (head.stopped && head.speed < 0.0) {
                head.current_time_code = if head.speed < 0.0 { head.end_time_code } else { head.start_time_code };
            }
            if !head.playing && (head.run_id == 0 || head.completed || head.stopped || finished || head.completed_run_id == Some(head.run_id)) {
                head.run_id = head.run_id.wrapping_add(1);
            }
            head.completed = false; head.stopped = false; head.playing = true;
        }
        ClipTransport::Pause => { head.playing = false; head.stopped = false; }
        ClipTransport::Stop => head.stop(),
        ClipTransport::Seek(tc) => { head.scrub_to(tc); head.completed = false; head.stopped = false; }
        ClipTransport::SetSpeed(speed) => head.speed = speed,
        ClipTransport::SetLooping(looping) => head.looping = looping,
        ClipTransport::Release => release_head(head, &mut restore),
    }
    drop(player);
    queue_restore(world, restore);
    for event in events { queue_event(world, event); }
    Ok(())
}

/// Read-only transform ownership query for the behavior host boundary.
pub fn owns_transform(world: &World, entity: Entity) -> bool {
    let Some(node) = world.get::<PrimNode>(entity) else { return false };
    let Some(mut players) = world.try_query::<&AnimClipPlayer>() else { return false };
    players.iter(world).any(|p| p.playheads.iter()
        .filter(|h| h.independent && h.error.is_empty()).any(|h| h.targets.iter().any(|target|
            world.iter_entities().any(|t| t.get::<PrimNode>().is_some_and(|n| n.prim == *target && within(&node.path, &n.path))))))
}
