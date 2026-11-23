//! Composition-aware animation discovery: the [`AnimationCatalog`].
//!
//! The catalog is a read-only index built from the live composed ECS world —
//! the per-prim [`AnimatedAttribute`] components the hydrator already populates
//! (ordinary time-sampled transforms/visibility/geometry plus skeletal sources)
//! plus the [`PrimNode`] path/identity tables. It answers "what animations does
//! this scope / this prim own?" without any live USD traversal, so it is safe to
//! build once per hydration and query from the editor and from the session's
//! playhead hydration.
//!
//! # What this slice covers
//!
//! Discovery produces one **anonymous entry per animated prim** (its own group
//! of tracks), mapped to its owning [`AnimationScope`] (composed stage or a
//! level holder subtree) and classified by [`AnimationKind`]. Named/tagged
//! entries, shared-source consumer resolution, and payload-boundary reporting
//! are authored-schema / FFI concerns deferred to later increments; the entry
//! shape already carries the `name`/`tags` fields so those can be filled in
//! without a breaking change.

use bevy_ecs::prelude::*;
use idtx_usd::PrimId;

use crate::components::{AnimatedAttribute, PrimNode, TrackTarget};

/// The compositional occurrence an animation belongs to.
///
/// Derived from the owner prim's composed path, mirroring the assembly layout
/// (`/World/Levels/<name>` default-prim payload holders, and the Scenario-2
/// `/World/level` reference body). Everything not under a level holder maps to
/// the whole composed stage.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub enum AnimationScope {
    /// The entire live composed stage.
    #[default]
    ComposedStage,
    /// A level holder subtree, addressed by its holder prim path
    /// (`/World/Levels/<name>` or `/World/level`).
    Level { holder_path: String },
}

impl AnimationScope {
    /// Resolve the owning scope of a composed prim path.
    #[must_use]
    pub fn for_path(path: &str) -> Self {
        if let Some(holder) = level_holder_of(path) {
            Self::Level { holder_path: holder }
        } else {
            Self::ComposedStage
        }
    }

    /// `true` when `path` lies within this scope.
    #[must_use]
    pub fn contains_path(&self, path: &str) -> bool {
        match self {
            Self::ComposedStage => true,
            Self::Level { holder_path } => path_is_within(path, holder_path),
        }
    }
}

/// Return the level-holder prefix of `path`, if it lies under one.
///
/// `/World/Levels/<name>/...` → `/World/Levels/<name>`; `/World/level/...` →
/// `/World/level`; anything else → `None`.
#[must_use]
pub fn level_holder_of(path: &str) -> Option<String> {
    if let Some(rest) = path.strip_prefix("/World/Levels/") {
        let name = rest.split('/').next().filter(|s| !s.is_empty())?;
        return Some(format!("/World/Levels/{name}"));
    }
    if path == "/World/level" || path.starts_with("/World/level/") {
        return Some("/World/level".to_owned());
    }
    None
}

/// Is `path` equal to, or a namespace descendant of, `ancestor`?
#[must_use]
fn path_is_within(path: &str, ancestor: &str) -> bool {
    path == ancestor
        || (path.len() > ancestor.len()
            && path.starts_with(ancestor)
            && path.as_bytes()[ancestor.len()] == b'/')
}

/// Coarse classification of an animation group, matching the session's
/// domain-based resample fast paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AnimationKind {
    /// All tracks are `xformOp:*` — the cheap transform-only resample path.
    Transform,
    /// All tracks drive a `UsdSkel` joint palette / bounds — the skeleton-pose
    /// resample path.
    Skeletal,
    /// Anything else (animated points, visibility, blend shapes, …) — needs a
    /// full viewport resample.
    Geometry,
}

fn is_skel_pose_attr(name: &str) -> bool {
    matches!(name, "rotations" | "translations" | "scales" | "extent")
}

fn classify_tracks(anim: &AnimatedAttribute) -> AnimationKind {
    if anim.tracks.is_empty() {
        return AnimationKind::Geometry;
    }
    let all_xform = anim.tracks.iter().all(|t| match &t.target {
        TrackTarget::Attribute { attr_name } => attr_name.starts_with("xformOp:"),
        _ => false,
    });
    if all_xform {
        return AnimationKind::Transform;
    }
    let all_skel = anim.tracks.iter().all(|t| match &t.target {
        TrackTarget::Attribute { attr_name } => is_skel_pose_attr(attr_name),
        TrackTarget::SkeletalJoint { .. } => true,
        TrackTarget::BlendShape { .. } => false,
    });
    if all_skel {
        AnimationKind::Skeletal
    } else {
        AnimationKind::Geometry
    }
}

/// One controllable animation group discovered on the composed stage.
#[derive(Debug, Clone, PartialEq)]
pub struct AnimationEntry {
    /// The prim that owns the tracks in this group.
    pub owner: PrimId,
    /// The owner's composed path.
    pub owner_path: String,
    /// The compositional occurrence this group belongs to.
    pub scope: AnimationScope,
    /// A stable, user-facing name for the group. `None` for an anonymous
    /// discovered group (addressed by [`AnimationEntry::label`] instead); a
    /// schema-free `idtx:animation:clipNames` declarations fill this in.
    pub name: Option<String>,
    /// Selection tags (empty for discovered groups).
    pub tags: Vec<String>,
    /// Coarse domain classification.
    pub kind: AnimationKind,
    /// Composed time-code range (the min/max over the group's tracks).
    pub start_time_code: f64,
    pub end_time_code: f64,
    /// Number of time-sampled tracks in the group.
    pub track_count: usize,
    /// Prims whose sampling this group affects. For a discovered per-prim group
    /// this is the owner itself; the session's skeletal sampler expands a
    /// skeletal root to its bound consumers.
    pub affected: Vec<PrimId>,
}

impl AnimationEntry {
    /// The label a transport command uses to address this group's playhead:
    /// the authored `name` if present, else the owner path.
    #[must_use]
    pub fn label(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.owner_path)
    }

    /// `true` when the group has a non-empty playable range.
    #[must_use]
    pub fn has_range(&self) -> bool {
        self.end_time_code > self.start_time_code
    }
}

/// A read-only index of every discovered animation group plus its scope.
#[derive(Debug, Clone, Default)]
pub struct AnimationCatalog {
    pub entries: Vec<AnimationEntry>,
}

/// Filter for [`AnimationCatalog::query`]. An unset field does not constrain.
#[derive(Debug, Clone, Default)]
pub struct AnimationQuery {
    /// Restrict to a single owning scope.
    pub scope: Option<AnimationScope>,
    /// Restrict to the owner prim at this path or its namespace descendants.
    pub owner_subtree: Option<String>,
    /// Restrict to groups with exactly this authored name.
    pub name: Option<String>,
    /// Restrict to one tag (present in [`AnimationEntry::tags`]).
    pub tag: Option<String>,
    /// Restrict to one coarse kind.
    pub kind: Option<AnimationKind>,
}

impl AnimationCatalog {
    /// Build the catalog from every entity carrying an [`AnimatedAttribute`].
    #[must_use]
    pub fn build(world: &World) -> Self {
        let mut entries = Vec::new();
        for e in world.iter_entities() {
            let entity = e.id();
            let Some(anim) = world.get::<AnimatedAttribute>(entity) else {
                continue;
            };
            let Some(node) = world.get::<PrimNode>(entity) else {
                continue;
            };
            if node.is_pseudo_root || anim.tracks.is_empty() {
                continue;
            }
            let start = anim
                .tracks
                .iter()
                .map(|t| t.start_time_code)
                .reduce(f64::min)
                .unwrap_or(0.0);
            let end = anim
                .tracks
                .iter()
                .map(|t| t.end_time_code)
                .reduce(f64::max)
                .unwrap_or(0.0);
            let mut affected = vec![node.prim];
            if node.type_name == "SkelAnimation" {
                let skeletons: std::collections::HashSet<_> = world.iter_entities().filter_map(|e|
                    e.get::<crate::SkeletonPoseComp>().filter(|p| p.record.animation_source == Some(node.prim))
                        .map(|p| p.record.skeleton)).collect();
                affected.extend(skeletons.iter().copied());
                affected.extend(world.iter_entities().filter_map(|e| e.get::<crate::SkinBindingComp>()
                    .filter(|s| skeletons.contains(&s.record.skeleton)).map(|s| s.record.prim)));
                if skeletons.is_empty() { affected.clear(); }
            }
            entries.push(AnimationEntry {
                owner: node.prim,
                owner_path: node.path.clone(),
                scope: AnimationScope::for_path(&node.path),
                name: None,
                tags: Vec::new(),
                kind: classify_tracks(anim),
                start_time_code: start,
                end_time_code: end,
                track_count: anim.tracks.len(),
                affected,
            });
        }
        for entity in world.iter_entities() {
            let (Some(node), Some(player)) = (entity.get::<PrimNode>(), entity.get::<crate::AnimClipPlayer>()) else { continue };
            for head in &player.playheads {
                let Some(declaration) = &head.declaration else { continue };
                entries.push(AnimationEntry {
                    owner: node.prim, owner_path: node.path.clone(), scope: AnimationScope::for_path(&node.path),
                    name: Some(head.clip.clone()), tags: Vec::new(),
                    kind: if declaration.source.is_empty() { AnimationKind::Geometry } else { AnimationKind::Skeletal },
                    start_time_code: declaration.start, end_time_code: declaration.end,
                    track_count: 0, affected: head.targets.clone(),
                });
            }
        }
        entries.sort_by(|a, b| a.owner_path.cmp(&b.owner_path).then_with(|| a.name.cmp(&b.name)));
        Self { entries }
    }

    /// Entries matching every set field of `q`.
    #[must_use]
    pub fn query(&self, q: &AnimationQuery) -> Vec<&AnimationEntry> {
        self.entries
            .iter()
            .filter(|e| q.scope.as_ref().is_none_or(|s| &e.scope == s))
            .filter(|e| {
                q.owner_subtree
                    .as_deref()
                    .is_none_or(|p| path_is_within(&e.owner_path, p))
            })
            .filter(|e| q.name.as_deref().is_none_or(|n| e.name.as_deref() == Some(n)))
            .filter(|e| q.tag.as_deref().is_none_or(|t| e.tags.iter().any(|x| x == t)))
            .filter(|e| q.kind.is_none_or(|k| e.kind == k))
            .collect()
    }

    /// Every entry within `scope`.
    #[must_use]
    pub fn in_scope(&self, scope: &AnimationScope) -> Vec<&AnimationEntry> {
        self.query(&AnimationQuery {
            scope: Some(scope.clone()),
            ..Default::default()
        })
    }

    /// The groups a prim "owns or consumes": the prim's own group plus any group
    /// on a namespace descendant (e.g. an asset root resolving the animations of
    /// its nested references/payloads).
    #[must_use]
    pub fn self_animations(&self, owner_path: &str) -> Vec<&AnimationEntry> {
        self.query(&AnimationQuery {
            owner_subtree: Some(owner_path.to_owned()),
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::AnimTrack;
    use idtx_usd::PrimState;
    use idtx_usd::StageGeneration;

    fn pid(slot: u32) -> PrimId {
        PrimId::from_raw_parts(slot, StageGeneration::from_raw(1))
    }

    fn node(slot: u32, path: &str) -> PrimNode {
        PrimNode {
            prim: pid(slot),
            path: path.to_owned(),
            parent: None,
            name: path.rsplit('/').next().unwrap_or("").to_owned(),
            type_name: "Xform".to_owned(),
            state: PrimState {
                active: true,
                defined: true,
                loaded: true,
            },
            is_pseudo_root: false,
            local_layer_identifier: None,
        }
    }

    fn xform_track() -> AnimTrack {
        AnimTrack {
            name: "xformOp:translate".to_owned(),
            target: TrackTarget::Attribute {
                attr_name: "xformOp:translate".to_owned(),
            },
            value_type: String::new(),
            start_time_code: 0.0,
            end_time_code: 24.0,
            sample_count: 2,
            interpolation: None,
        }
    }

    fn points_track() -> AnimTrack {
        AnimTrack {
            name: "points".to_owned(),
            target: TrackTarget::Attribute {
                attr_name: "points".to_owned(),
            },
            value_type: String::new(),
            start_time_code: 0.0,
            end_time_code: 48.0,
            sample_count: 2,
            interpolation: None,
        }
    }

    #[test]
    fn scope_for_path_maps_level_holders() {
        assert_eq!(
            AnimationScope::for_path("/World/Levels/Cave/Door"),
            AnimationScope::Level {
                holder_path: "/World/Levels/Cave".to_owned()
            }
        );
        assert_eq!(
            AnimationScope::for_path("/World/level/Box"),
            AnimationScope::Level {
                holder_path: "/World/level".to_owned()
            }
        );
        assert_eq!(
            AnimationScope::for_path("/World/Prop"),
            AnimationScope::ComposedStage
        );
        // Not fooled by a prefix that isn't a path boundary.
        assert_eq!(
            AnimationScope::for_path("/World/leveling"),
            AnimationScope::ComposedStage
        );
    }

    #[test]
    fn build_classifies_and_scopes_entries() {
        let mut world = World::new();
        world.spawn((
            node(1, "/World/Levels/Cave/Door"),
            AnimatedAttribute {
                tracks: vec![xform_track()],
            },
        ));
        world.spawn((
            node(2, "/World/Fan"),
            AnimatedAttribute {
                tracks: vec![points_track()],
            },
        ));
        // Animated attr with no PrimNode, and pseudo-root, are skipped.
        world.spawn(AnimatedAttribute { tracks: vec![xform_track()] });

        let cat = AnimationCatalog::build(&world);
        assert_eq!(cat.entries.len(), 2);

        let find = |p: &str| cat.entries.iter().find(|e| e.owner_path == p).unwrap();
        let door = find("/World/Levels/Cave/Door");
        assert_eq!(door.kind, AnimationKind::Transform);
        assert_eq!(
            door.scope,
            AnimationScope::Level {
                holder_path: "/World/Levels/Cave".to_owned()
            }
        );
        assert!((door.end_time_code - 24.0).abs() < 1e-9);
        assert_eq!(door.label(), "/World/Levels/Cave/Door");

        let fan = find("/World/Fan");
        assert_eq!(fan.kind, AnimationKind::Geometry);
        assert_eq!(fan.scope, AnimationScope::ComposedStage);
    }

    #[test]
    fn query_filters_compose() {
        let mut world = World::new();
        world.spawn((
            node(1, "/World/Levels/Cave/Door"),
            AnimatedAttribute { tracks: vec![xform_track()] },
        ));
        world.spawn((
            node(2, "/World/Levels/Cave/Door/Leaf"),
            AnimatedAttribute { tracks: vec![points_track()] },
        ));
        world.spawn((
            node(3, "/World/Fan"),
            AnimatedAttribute { tracks: vec![xform_track()] },
        ));
        let cat = AnimationCatalog::build(&world);

        let cave = cat.in_scope(&AnimationScope::Level {
            holder_path: "/World/Levels/Cave".to_owned(),
        });
        assert_eq!(cave.len(), 2);

        let under_door = cat.self_animations("/World/Levels/Cave/Door");
        assert_eq!(under_door.len(), 2, "owner + descendant Leaf");

        let xforms = cat.query(&AnimationQuery {
            kind: Some(AnimationKind::Transform),
            ..Default::default()
        });
        assert_eq!(xforms.len(), 2); // Door + Fan
    }
}
