//! Spring bone tuning hooks. VRMC joint parameters are edited live from the
//! **Rig editor** debug window (`SpringJointProps` on loaded VRMs).
//!
//! Optional per-VRM preset auto-load (feature-flagged via `[avatar].auto_load_spring_preset`)
//! runs in [`crate::plugins::spring_preset::auto_load_spring_preset_system`].

use bevy::prelude::*;

use super::spring_preset::auto_load_spring_preset_system;

pub struct SpringBonePlugin;

impl Plugin for SpringBonePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, auto_load_spring_preset_system);
    }
}
