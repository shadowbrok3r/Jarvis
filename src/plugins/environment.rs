//! Global ambient + directional light and optional ground plane.

use bevy::light::GlobalAmbientLight;
use bevy::prelude::*;

use jarvis_avatar::config::Settings;

pub struct EnvironmentPlugin;

impl Plugin for EnvironmentPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup_environment);
    }
}

/// Tag for the sun entity so the debug UI can tweak it live.
#[derive(Component)]
pub struct SunLight;

/// Tag for the ground plane + its material handle for live recolor/resize.
#[derive(Component)]
pub struct GroundPlane {
    pub material: Handle<StandardMaterial>,
    pub mesh: Handle<Mesh>,
}

fn setup_environment(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut global_ambient: ResMut<GlobalAmbientLight>,
    settings: Res<Settings>,
) {
    let g = &settings.graphics;
    let [r, gc, b, a] = g.ambient_color;
    *global_ambient = GlobalAmbientLight {
        color: Color::linear_rgba(r, gc, b, a),
        brightness: g.ambient_brightness,
        affects_lightmapped_meshes: true,
    };

    // The key light is spawned by `LightRigPlugin` (and tagged `SunLight` so the
    // debug UI's sun controls still find it). If you want the old single-sun
    // setup, disable `light_rig` in config and re-enable the block below.
    let _ = (g.directional_position, g.directional_look_at);
    let _ = g.directional_illuminance;
    let _ = g.directional_shadows;

    let half = g.ground_size * 0.5;
    let mesh = meshes.add(Plane3d::new(Vec3::Y, Vec2::new(half, half)));
    let mat = materials.add(StandardMaterial {
        base_color: {
            let [r, gc, b] = g.ground_base_color;
            Color::linear_rgb(r, gc, b)
        },
        perceptual_roughness: 0.95,
        metallic: 0.0,
        ..default()
    });
    commands.spawn((
        Mesh3d(mesh.clone()),
        MeshMaterial3d(mat.clone()),
        Transform::IDENTITY,
        if g.show_ground_plane {
            Visibility::Visible
        } else {
            Visibility::Hidden
        },
        GroundPlane {
            material: mat,
            mesh,
        },
    ));
}
