//! Four-point anime light rig (key / fill / rim / back) driven by
//! `Settings::light_rig`. Replaces the single hard-coded sun. MToon (patched
//! in vendored `bevy_vrm1`) adds shadowed directionals with shadow maps and
//! non-shadow directionals as extra unshadowed N·L terms, so fill/rim/back still
//! shape the toon ramp without paying for four shadow atlases.
//!
//! Settings mutations re-sync every frame so the Graphics Advanced window
//! can tweak illuminance / colour / direction live. Optional
//! [`ShowLightGizmo`] markers draw Blender-style direction arrows at a rig
//! focus anchor (translation is visual-only for directionals).

use std::collections::HashSet;

use bevy::gizmos::light::{LightGizmoConfigGroup, ShowLightGizmo};
use bevy::gizmos::prelude::GizmoConfigStore;
use bevy::prelude::*;
use bevy_vrm1::prelude::Vrm;

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
        // `LightGizmoPlugin` is already registered by Bevy's `GizmoPlugin` (DefaultPlugins).
        app.add_systems(Startup, spawn_light_rig)
            .add_systems(
                Update,
                (
                    ensure_light_rig_entities,
                    sync_light_rig,
                    sync_light_gizmo_visibility,
                )
                    .chain(),
            );
    }
}

/// Role of a light in the rig (used as lookup key so we can re-sync from
/// [`Settings`] without spawning duplicates).
#[derive(Component, Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum LightRigRole {
    Key,
    Fill,
    Rim,
    Back,
}

fn spawn_light_rig(mut commands: Commands, settings: Res<Settings>) {
    if !settings.light_rig.enabled {
        return;
    }
    spawn_all_rig_lights(&mut commands, &settings.light_rig);
}

fn spawn_all_rig_lights(commands: &mut Commands, rig: &jarvis_avatar::config::LightRigSettings) {
    spawn_one(commands, LightRigRole::Key, &rig.key);
    spawn_one(commands, LightRigRole::Fill, &rig.fill);
    spawn_one(commands, LightRigRole::Rim, &rig.rim);
    spawn_one(commands, LightRigRole::Back, &rig.back);
}

/// Spawns any rig role added in config after an older build (e.g. the new back light).
fn ensure_light_rig_entities(
    mut commands: Commands,
    settings: Res<Settings>,
    existing: Query<&LightRigRole>,
) {
    if !settings.light_rig.enabled {
        return;
    }
    let have: HashSet<LightRigRole> = existing.iter().copied().collect();
    let rig = &settings.light_rig;
    if !have.contains(&LightRigRole::Key) {
        spawn_one(&mut commands, LightRigRole::Key, &rig.key);
    }
    if !have.contains(&LightRigRole::Fill) {
        spawn_one(&mut commands, LightRigRole::Fill, &rig.fill);
    }
    if !have.contains(&LightRigRole::Rim) {
        spawn_one(&mut commands, LightRigRole::Rim, &rig.rim);
    }
    if !have.contains(&LightRigRole::Back) {
        spawn_one(&mut commands, LightRigRole::Back, &rig.back);
    }
}

fn spawn_one(commands: &mut Commands, role: LightRigRole, spec: &LightSpec) {
    let direction = Vec3::from_array(spec.direction).normalize_or_zero();
    let transform = light_transform(direction, Vec3::ZERO, 0.0);
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
    if matches!(role, LightRigRole::Key) {
        ent.insert(SunLight);
    }
}

/// World transform for a directional: rotation aims -Z along `direction`;
/// optional `anchor` offsets the gizmo origin (ignored by the light itself).
fn light_transform(direction: Vec3, anchor: Vec3, gizmo_distance: f32) -> Transform {
    if direction.length_squared() > 0.0 {
        let dir = direction.normalize();
        let pos = anchor + dir * gizmo_distance;
        Transform::from_translation(pos).looking_to(dir, Vec3::Y)
    } else {
        Transform::from_translation(anchor)
    }
}

fn rig_focus(settings: &Settings, vrm_tf: Option<&GlobalTransform>) -> Vec3 {
    if settings.light_rig.use_avatar_focus_for_gizmos {
        if let Some(tf) = vrm_tf {
            return tf.translation() + Vec3::Y * settings.camera.focus_y_lift;
        }
    }
    Vec3::from_array(settings.camera.focus) + Vec3::Y * settings.camera.focus_y_lift
}

fn sync_light_rig(
    settings: Res<Settings>,
    vrm_tf: Query<&GlobalTransform, With<Vrm>>,
    mut query: Query<(
        &LightRigRole,
        &mut DirectionalLight,
        &mut Transform,
        &mut Visibility,
    )>,
) {
    let focus = rig_focus(
        &settings,
        vrm_tf.single().ok(),
    );
    let gizmo_dist = settings.light_rig.gizmo_distance.max(0.1);

    for (role, mut light, mut tf, mut vis) in &mut query {
        let spec = match role {
            LightRigRole::Key => &settings.light_rig.key,
            LightRigRole::Fill => &settings.light_rig.fill,
            LightRigRole::Rim => &settings.light_rig.rim,
            LightRigRole::Back => &settings.light_rig.back,
        };
        let effective = settings.light_rig.enabled && spec.enabled;
        *vis = vis_for(effective);
        light.color = Color::linear_rgb(spec.color[0], spec.color[1], spec.color[2]);
        light.illuminance = spec.illuminance;
        light.shadows_enabled = spec.shadows;
        let direction = Vec3::from_array(spec.direction).normalize_or_zero();
        *tf = light_transform(direction, focus, gizmo_dist);
    }
}

fn sync_light_gizmo_visibility(
    settings: Res<Settings>,
    mut commands: Commands,
    mut store: ResMut<GizmoConfigStore>,
    lights: Query<(Entity, &LightRigRole), With<DirectionalLight>>,
) {
    let show = settings.light_rig.enabled && settings.light_rig.show_light_gizmos;
    let (gizmo_config, _) = store.config_mut::<LightGizmoConfigGroup>();
    gizmo_config.enabled = show;

    for (entity, role) in &lights {
        if show {
            let color = match role {
                LightRigRole::Key => Color::linear_rgb(1.0, 0.92, 0.75),
                LightRigRole::Fill => Color::linear_rgb(0.55, 0.75, 1.0),
                LightRigRole::Rim => Color::linear_rgb(1.0, 0.7, 0.55),
                LightRigRole::Back => Color::linear_rgb(0.75, 0.85, 1.0),
            };
            commands.entity(entity).insert(ShowLightGizmo {
                color: Some(bevy::gizmos::light::LightGizmoColor::Manual(color)),
                ..default()
            });
        } else {
            commands.entity(entity).remove::<ShowLightGizmo>();
        }
    }
}
