//! Schema-free, versioned authored animation policy. Time endpoints are USD
//! `timecode` values; native composition resolves them before they cross FFI.
use crate::{StageError, StageService, ViewportProjection};
use idtx_usd_sys::bridge::ffi;
use std::time::Instant;

pub const ANIMATION_VERSION: &str = "idtx:animation:contractVersion";
pub const ANIMATION_CLIP_NAMES: &str = "idtx:animation:clipNames";

#[derive(Debug, Clone, PartialEq)]
pub struct AnimationClip {
    pub owner: String,
    pub name: String,
    pub source: String,
    pub targets: Vec<String>,
    pub start: f64,
    pub end: f64,
    pub speed: f64,
    pub looping: bool,
    pub autoplay: bool,
    pub channel: String,
    pub error: String,
}

impl AnimationClip {
    pub fn key(&self) -> String { format!("{}#{}", self.owner, self.name) }
}

impl StageService {
    /// Returns invalid entries too, for inspection and actionable runtime errors.
    pub fn animation_clips(&self, owner: &str) -> Result<Vec<AnimationClip>, StageError> {
        let mut clips: Vec<_> = ffi::read_animation_clips(self.handle_ref()?, owner)?
            .into_iter().map(|r| AnimationClip {
                owner: owner.to_owned(), name: r.name, source: r.source,
                targets: r.targets, start: r.start, end: r.end, speed: r.speed,
                looping: r.looping, autoplay: r.autoplay, channel: r.channel, error: r.error,
            }).collect();
        let conflicts: std::collections::HashSet<_> = clips.iter()
            .filter(|c| c.autoplay)
            .filter(|c| clips.iter().filter(|p| p.autoplay && p.channel == c.channel).count() > 1)
            .map(|c| c.channel.clone()).collect();
        for clip in &mut clips {
            if clip.autoplay && conflicts.contains(&clip.channel) {
                clip.error = format!("multiple autoplay actions on channel {}", clip.channel);
            }
        }
        Ok(clips)
    }

    /// Prepare and validate a private native binding. It shares source layers
    /// with this stage but its source opinions are never part of persisted USD.
    pub fn prepare_animation_clip(&self, clip: &AnimationClip) -> Result<(), StageError> {
        if !clip.error.is_empty() { return Err(StageError::OpenUsd(clip.error.clone())); }
        ffi::prepare_animation_binding(self.handle_ref()?, &clip.key(), &clip.owner,
            &clip.source, &clip.targets)?;
        Ok(())
    }

    pub fn release_animation_clip(&self, key: &str) -> Result<(), StageError> {
        ffi::release_animation_binding(self.handle_ref()?, key)?;
        Ok(())
    }

    pub fn sample_animation_clip(&mut self, clip: &AnimationClip, tc: f64, poses_only: bool)
        -> Result<ViewportProjection, StageError>
    {
        self.sample_animation_clip_with_placement(clip, tc, tc, poses_only)
    }

    pub fn sample_animation_clip_with_placement(&mut self, clip: &AnimationClip, tc: f64, placement_tc: f64, poses_only: bool)
        -> Result<ViewportProjection, StageError>
    {
        self.prepare_animation_clip(clip)?;
        let started = Instant::now();
        let raw = ffi::project_animation_binding(self.handle_ref()?, &clip.key(), tc, placement_tc, u32::from(poses_only))?;
        self.decode_viewport(raw, Some(tc), started)
    }

    pub fn sample_animation_clip_transforms(&mut self, clip: &AnimationClip, tc: f64, placement_tc: f64)
        -> Result<ViewportProjection, StageError>
    {
        self.prepare_animation_clip(clip)?;
        let started = Instant::now();
        let raw = ffi::project_animation_binding(self.handle_ref()?, &clip.key(), tc, placement_tc, 2)?;
        self.decode_viewport(raw, Some(tc), started)
    }

    /// Used by cook as well as runtime: inactive actions must also remain valid.
    pub fn validate_animation_clips(&self) -> Result<(), StageError> {
        for node in self.topology() {
            let Some(path) = self.path_for(node.prim) else { continue };
            for clip in self.animation_clips(path)? {
                let result = self.prepare_animation_clip(&clip);
                self.release_animation_clip(&clip.key())?;
                result.map_err(|e| StageError::InvalidProjection {
                    path: clip.owner.clone(), detail: format!("clip {}: {e}", clip.name),
                })?;
            }
        }
        Ok(())
    }
}
