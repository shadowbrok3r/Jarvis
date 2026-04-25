//! Spring bone tuning hooks. VRMC joint parameters are edited live from the
//! **Rig editor** debug window (`SpringJointProps` on loaded VRMs).

use bevy::prelude::*;

pub struct SpringBonePlugin;

impl Plugin for SpringBonePlugin {
    fn build(&self, _app: &mut App) {}
}
