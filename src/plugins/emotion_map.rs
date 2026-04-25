//! Loads [`EmotionMap`] from disk on startup and exposes it as a Bevy
//! resource so the dispatcher (see `expressions.rs`) + debug UI can read
//! and mutate the same table.
//!
//! Disk writes are driven by the UI's "Save" button; this plugin only
//! hydrates and inserts the resource.

use bevy::prelude::*;

use jarvis_avatar::config::Settings;
use jarvis_avatar::emotions::{resolve_emotions_path, EmotionMap};

pub struct EmotionMapPlugin;

impl Plugin for EmotionMapPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, load_emotion_map);
    }
}

/// Bevy `Resource` wrapper so we can keep [`EmotionMap`]'s public surface
/// plain-old data and reuse it from non-Bevy contexts (tests, CLI tools).
#[derive(Resource, Default)]
pub struct EmotionMapRes {
    pub inner: EmotionMap,
    /// UI status string (shows up under the Save button).
    pub last_status: Option<String>,
}

impl EmotionMapRes {
    pub fn save(&mut self) {
        match self.inner.save() {
            Ok(()) => {
                self.last_status = Some(format!(
                    "saved → {}",
                    self.inner.path.display()
                ));
            }
            Err(e) => {
                self.last_status = Some(format!("save failed: {e}"));
            }
        }
    }

    pub fn reload(&mut self) {
        let path = self.inner.path.clone();
        self.inner = EmotionMap::load_or_default(path);
        self.last_status = Some("reloaded from disk".into());
    }
}

fn load_emotion_map(mut commands: Commands, settings: Res<Settings>) {
    let path = resolve_emotions_path(&settings.emotions.path);
    let inner = EmotionMap::load_or_default(&path);
    info!(
        "emotion map loaded: {} mappings from {}",
        inner.mappings.len(),
        path.display()
    );
    commands.insert_resource(EmotionMapRes {
        inner,
        last_status: None,
    });
}
