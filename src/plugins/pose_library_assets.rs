//! Bevy-side wrapper around [`PoseLibrary`] that keeps a cached list of
//! poses / animation metadata accessible to egui (which runs on the main
//! thread and can't touch `std::fs` every frame without hitching).
//!
//! The UI calls into [`PoseLibraryAssets`] for reads; disk mutations (save /
//! delete / rename / recategorize) go through the same resource and flag the
//! cache dirty — [`refresh_pose_library`] re-reads on the next tick.

use std::sync::Arc;
use std::time::Duration;

use bevy::prelude::*;
use parking_lot::RwLock;

use jarvis_avatar::config::Settings;
use jarvis_avatar::paths::expand_home;
use jarvis_avatar::pose_library::{AnimationMeta, PoseFile, PoseLibrary};

pub struct PoseLibraryAssetsPlugin;

impl Plugin for PoseLibraryAssetsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, insert_library_resource)
            .add_systems(Update, refresh_pose_library);
    }
}

/// Cloneable cache around [`PoseLibrary`].
#[derive(Resource, Clone)]
pub struct PoseLibraryAssets {
    pub library: Arc<PoseLibrary>,
    poses: Arc<RwLock<Vec<PoseFile>>>,
    animations: Arc<RwLock<Vec<AnimationMeta>>>,
    dirty: Arc<RwLock<bool>>,
    last_error: Arc<RwLock<Option<String>>>,
    last_refresh: Arc<RwLock<Option<std::time::Instant>>>,
}

impl PoseLibraryAssets {
    pub fn new(library: PoseLibrary) -> Self {
        Self {
            library: Arc::new(library),
            poses: Arc::new(RwLock::new(Vec::new())),
            animations: Arc::new(RwLock::new(Vec::new())),
            dirty: Arc::new(RwLock::new(true)),
            last_error: Arc::new(RwLock::new(None)),
            last_refresh: Arc::new(RwLock::new(None)),
        }
    }

    pub fn poses(&self) -> Vec<PoseFile> {
        self.poses.read().clone()
    }

    pub fn animations(&self) -> Vec<AnimationMeta> {
        self.animations.read().clone()
    }

    pub fn mark_dirty(&self) {
        *self.dirty.write() = true;
    }

    pub fn last_error(&self) -> Option<String> {
        self.last_error.read().clone()
    }

    pub fn clear_error(&self) {
        *self.last_error.write() = None;
    }

    pub fn set_error(&self, msg: impl Into<String>) {
        *self.last_error.write() = Some(msg.into());
    }
}

fn insert_library_resource(mut commands: Commands, settings: Res<Settings>) {
    let poses_dir = expand_home(&settings.pose_library.poses_dir);
    let animations_dir = expand_home(&settings.pose_library.animations_dir);
    let library = PoseLibrary::new(poses_dir, animations_dir);
    commands.insert_resource(PoseLibraryAssets::new(library));
}

fn refresh_pose_library(assets: Option<Res<PoseLibraryAssets>>) {
    let Some(assets) = assets else {
        return;
    };
    let now = std::time::Instant::now();
    let should_refresh = {
        let mut dirty = assets.dirty.write();
        let stale = assets
            .last_refresh
            .read()
            .map(|t| now.duration_since(t) > Duration::from_secs(3))
            .unwrap_or(true);
        let out = *dirty || stale;
        if *dirty {
            *dirty = false;
        }
        out
    };
    if !should_refresh {
        return;
    }

    match assets.library.load_all_poses() {
        Ok(mut p) => {
            p.sort_by(|a, b| {
                a.name
                    .to_ascii_lowercase()
                    .cmp(&b.name.to_ascii_lowercase())
            });
            *assets.poses.write() = p;
        }
        Err(e) => assets.set_error(format!("load_all_poses: {e}")),
    }
    match assets.library.list_animations() {
        Ok(mut a) => {
            a.sort_by(|x, y| {
                x.name
                    .to_ascii_lowercase()
                    .cmp(&y.name.to_ascii_lowercase())
            });
            *assets.animations.write() = a;
        }
        Err(e) => assets.set_error(format!("list_animations: {e}")),
    }
    *assets.last_refresh.write() = Some(now);
}
