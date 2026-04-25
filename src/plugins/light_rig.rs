//! Three-point anime light rig (key / fill / rim) driven by
//! `Settings::light_rig`. Replaces the single hard-coded sun. MToon (patched
//! in vendored `bevy_vrm1`) adds shadowed directionals with shadow maps and
//! non-shadow directionals as extra unshadowed N·L terms, so fill/rim still
//! shape the toon ramp without paying for three shadow atlases.
//!
//! Settings mutations re-sync every frame so the Graphics Advanced window
//! can tweak illuminance / colour / direction live.

use bevy::prelude::*;

use jarvis_avatar::config::{LightSpec, Settings};

use crate::plugins::environment::SunLight;

fn vis_for(enabled: bool) -> Visibility {
    if enabled {
        Visibility::Visible
    } else {
        Visibility::Hidden
    }
}

pub struct LightRigPlugin;

impl Plugin for LightRigPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_light_rig)
            .add_systems(Update, sync_light_rig);
    }
}

/// Role of a light in the rig (used as lookup key so we can re-sync from
/// [`Settings`] without spawning duplicates).
#[derive(Component, Copy, Clone, Eq, PartialEq, Debug)]
pub enum LightRigRole {
    Key,
    Fill,
    Rim,
}

fn spawn_light_rig(mut commands: Commands, settings: Res<Settings>) {
    if !settings.light_rig.enabled {
        return;
    }
    spawn_one(&mut commands, LightRigRole::Key, &settings.light_rig.key);
    spawn_one(&mut commands, LightRigRole::Fill, &settings.light_rig.fill);
    spawn_one(&mut commands, LightRigRole::Rim, &settings.light_rig.rim);
}

fn spawn_one(commands: &mut Commands, role: LightRigRole, spec: &LightSpec) {
    let direction = Vec3::from_array(spec.direction).normalize_or_zero();
    let transform = if direction.length_squared() > 0.0 {
        Transform::IDENTITY.looking_to(direction, Vec3::Y)
    } else {
        Transform::IDENTITY
    };
    let mut ent = commands.spawn((
        DirectionalLight {
            color: Color::linear_rgb(spec.color[0], spec.color[1], spec.color[2]),
            illuminance: spec.illuminance,
            shadows_enabled: spec.shadows,
            ..default()
        },
        transform,
        vis_for(spec.enabled),
        role,
    ));
    // The key light doubles as `SunLight` so the existing debug UI's sun
    // sliders continue to target the dominant light.
    if matches!(role, LightRigRole::Key) {
        ent.insert(SunLight);
    }
}

fn sync_light_rig(
    settings: Res<Settings>,
    mut query: Query<(
        &LightRigRole,
        &mut DirectionalLight,
        &mut Transform,
        &mut Visibility,
    )>,
) {
    // Always push rig state into components. `Settings::is_changed()` is easy to miss
    // when the Graphics Advanced egui pass runs in a different schedule than `Update`,
    // which made illuminance sliders appear dead while enable/disable still toggled visibility.
    for (role, mut light, mut tf, mut vis) in &mut query {
        let spec = match role {
            LightRigRole::Key => &settings.light_rig.key,
            LightRigRole::Fill => &settings.light_rig.fill,
            LightRigRole::Rim => &settings.light_rig.rim,
        };
        let effective = settings.light_rig.enabled && spec.enabled;
        *vis = vis_for(effective);
        light.color = Color::linear_rgb(spec.color[0], spec.color[1], spec.color[2]);
        light.illuminance = spec.illuminance;
        light.shadows_enabled = spec.shadows;
        let direction = Vec3::from_array(spec.direction).normalize_or_zero();
        if direction.length_squared() > 0.0 {
            *tf = Transform::IDENTITY.looking_to(direction, Vec3::Y);
        }
    }
}
