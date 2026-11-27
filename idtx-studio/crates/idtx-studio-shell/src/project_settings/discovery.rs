//! One bounded background discovery worker. UI frames only consume snapshots.
use super::{project_usd_files, root_prim_options};
use idtx_editor_ui::project_settings::ProjectSettingsState;
use idtx_project::IdtxProject;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::time::SystemTime;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Key {
    root: PathBuf,
    revision: u64,
    selected: Vec<String>,
}

struct Request {
    key: Key,
    project: IdtxProject,
    repaint: egui::Context,
}

#[derive(Debug)]
struct Snapshot {
    key: Key,
    assets: Vec<String>,
    prims: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Default)]
pub(crate) struct DiscoveryCache {
    revision: u64,
    worker: Option<(SyncSender<Request>, Receiver<Snapshot>)>,
    pending: bool,
    snapshot: Option<Snapshot>,
    #[cfg(test)]
    requests: usize,
}

impl DiscoveryCache {
    pub fn invalidate(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    pub fn update(
        &mut self,
        project: &IdtxProject,
        dialog: &mut ProjectSettingsState,
        ctx: &egui::Context,
    ) {
        let mut selected: Vec<_> = dialog
            .runtime_prototypes
            .iter()
            .map(|row| row.asset.trim().to_owned())
            .filter(|asset| !asset.is_empty())
            .collect();
        selected.sort();
        selected.dedup();
        let key = Key {
            root: project.root().to_owned(),
            revision: self.revision,
            selected,
        };
        if let Some((_, rx)) = &self.worker {
            match rx.try_recv() {
                Ok(snapshot) => {
                    self.pending = false;
                    if snapshot.key == key {
                        self.snapshot = Some(snapshot);
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.pending = false;
                    self.worker = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        if self
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.key.root != key.root)
        {
            self.snapshot = None;
        }
        if let Some(snapshot) = &self.snapshot {
            dialog.asset_options.clone_from(&snapshot.assets);
            dialog.stage_options = snapshot
                .assets
                .iter()
                .filter(|p| p.strip_prefix("stages/").is_some_and(|p| !p.contains('/')))
                .cloned()
                .collect();
            for row in &mut dialog.runtime_prototypes {
                row.prim_options = snapshot
                    .prims
                    .get(row.asset.trim())
                    .cloned()
                    .unwrap_or_default();
            }
        } else {
            dialog.asset_options.clear();
            dialog.stage_options.clear();
            for row in &mut dialog.runtime_prototypes {
                row.prim_options.clear();
            }
        }
        let changed = self
            .snapshot
            .as_ref()
            .is_none_or(|snapshot| snapshot.key != key);
        if !self.pending && changed {
            if self.worker.is_none() {
                self.worker = Some(spawn_worker());
            }
            let request = Request {
                key,
                project: project.clone(),
                repaint: ctx.clone(),
            };
            if self.worker.as_ref().unwrap().0.try_send(request).is_ok() {
                self.pending = true;
                #[cfg(test)]
                {
                    self.requests += 1;
                }
            }
        }
        // Opening, selection changes, saved/imported assets and explicit refresh
        // drive invalidation. Idle frames never dispatch filesystem discovery.
    }
}

#[derive(Default)]
struct WorkerCache {
    project: Option<(PathBuf, u64)>,
    prims: BTreeMap<String, (Option<(u64, SystemTime)>, Vec<String>)>,
    #[cfg(test)]
    reads: usize,
}

impl WorkerCache {
    fn discover(&mut self, project: &IdtxProject, key: Key) -> Snapshot {
        let identity = (key.root.clone(), key.revision);
        if self.project.as_ref() != Some(&identity) {
            self.prims.clear();
            self.project = Some(identity);
        }
        let assets = project_usd_files(project);
        self.prims.retain(|asset, _| key.selected.contains(asset));
        for asset in &key.selected {
            let revision = project
                .project_root()
                .resolve_relative(std::path::Path::new(asset))
                .and_then(|path| std::fs::metadata(path).ok())
                .and_then(|metadata| {
                    metadata
                        .modified()
                        .ok()
                        .map(|mtime| (metadata.len(), mtime))
                });
            if self
                .prims
                .get(asset)
                .is_none_or(|(old, _)| old != &revision)
            {
                self.prims
                    .insert(asset.clone(), (revision, root_prim_options(project, asset)));
                #[cfg(test)]
                {
                    self.reads += 1;
                }
            }
        }
        Snapshot {
            key,
            assets,
            prims: self
                .prims
                .iter()
                .map(|(asset, (_, prims))| (asset.clone(), prims.clone()))
                .collect(),
        }
    }
}

fn spawn_worker() -> (SyncSender<Request>, Receiver<Snapshot>) {
    let (tx, requests) = sync_channel::<Request>(1);
    let (results, rx) = sync_channel(1);
    std::thread::spawn(move || {
        let mut cache = WorkerCache::default();
        while let Ok(request) = requests.recv() {
            let snapshot = cache.discover(&request.project, request.key);
            if results.send(snapshot).is_err() {
                break;
            }
            request.repaint.request_repaint();
        }
    });
    (tx, rx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_frames_and_unchanged_asset_revisions_do_not_repeat_usd_discovery() {
        let dir = tempfile::tempdir().unwrap();
        let project =
            IdtxProject::create(dir.path(), idtx_project::ProjectManifest::new("cache")).unwrap();
        let asset = project.root().join("panel.usda");
        std::fs::write(&asset, "#usda 1.0\ndef Xform \"Panel\" {}\n").unwrap();
        let key = Key {
            root: project.root().to_owned(),
            revision: 0,
            selected: vec!["panel.usda".into()],
        };
        let mut worker = WorkerCache::default();
        let snapshot = worker.discover(&project, key.clone());
        assert_eq!(snapshot.prims["panel.usda"], ["/Panel"]);
        worker.discover(&project, key.clone());
        assert_eq!(worker.reads, 1);
        let mut ui = DiscoveryCache {
            snapshot: Some(snapshot),
            ..Default::default()
        };
        let mut dialog = super::super::state_from_project(&project);
        dialog
            .runtime_prototypes
            .push(idtx_editor_ui::project_settings::RuntimePrototypeRow {
                asset: "panel.usda".into(),
                ..Default::default()
            });
        for _ in 0..120 {
            ui.update(&project, &mut dialog, &egui::Context::default());
        }
        assert_eq!(
            ui.requests, 0,
            "quiet UI frames must not dispatch discovery"
        );
        assert_eq!(dialog.runtime_prototypes[0].prim_options, ["/Panel"]);
        std::fs::write(&asset, "#usda 1.0\ndef Xform \"RenamedPanel\" {}\n").unwrap();
        assert_eq!(
            worker.discover(&project, key.clone()).prims["panel.usda"],
            ["/RenamedPanel"]
        );
        assert_eq!(worker.reads, 2);
        let other = tempfile::tempdir().unwrap();
        let other =
            IdtxProject::create(other.path(), idtx_project::ProjectManifest::new("other")).unwrap();
        ui.update(&other, &mut dialog, &egui::Context::default());
        assert!(
            dialog.asset_options.is_empty(),
            "project switching cannot expose an old snapshot"
        );
        assert_eq!(ui.requests, 1);
    }
}
