//! Tier-2 post-processing (tonemap + bloom + AA + env map + auto-exposure)
//! applied to the main `Camera3d` via `Settings::graphics.advanced`.
//!
//! The plugin idempotently inserts / removes components whenever settings
//! change so the Graphics Advanced window can toggle effects live. HDR is
//! required for bloom / tonemapping to be perceptible, so the plugin
//! respects `Settings::graphics.hdr` and simply no-ops when it's off.

use bevy::anti_alias::{
    fxaa::Fxaa,
    smaa::{Smaa, SmaaPreset},
};
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::light::EnvironmentMapLight;
use bevy::pbr::{ScreenSpaceAmbientOcclusion, ScreenSpaceAmbientOcclusionQualityLevel};
use bevy::post_process::{
    auto_exposure::AutoExposure,
    bloom::{Bloom, BloomCompositeMode, BloomPrefilter},
};
use bevy::asset::{AssetLoadFailedEvent, LoadState};
use bevy::prelude::*;
use bevy::render::render_resource::{TextureDimension, TextureFormat, TextureViewDimension};

use crate::config::{GraphicsAdvancedSettings, Settings, msaa_allows_ssao};

/// Live status for the Graphics Advanced environment-map row.
#[derive(Resource, Clone, Default)]
pub struct EnvironmentMapStatus {
    pub stem: String,
    pub diffuse_loaded: bool,
    pub specular_loaded: bool,
    pub diffuse_load_state: String,
    pub specular_load_state: String,
    pub diffuse_is_cubemap: bool,
    pub diffuse_format: String,
    pub diffuse_data_bytes: usize,
    pub diffuse_sample_luminance: f32,
    pub attached: bool,
    /// Target nits from settings (intensity × mtoon_boost).
    pub intensity_nits: f32,
    /// Value last written to the camera [`EnvironmentMapLight`] (should match target).
    pub camera_intensity_nits: f32,
    pub rotation_yaw_deg: f32,
    pub effective_ambient_brightness: f32,
    /// True when maps look valid on CPU; GPU may still be one frame behind.
    pub maps_look_valid: bool,
    pub message: String,
}

#[derive(Component)]
struct EnvironmentMapDebugSphere;

#[derive(Resource, Default)]
struct EnvironmentMapDebugSphereEntity(Option<Entity>);

#[derive(Resource, Default)]
struct EnvironmentMapStableHandles {
    diffuse: Option<Handle<Image>>,
    specular: Option<Handle<Image>>,
}

pub struct GraphicsAdvancedPlugin;

impl Plugin for GraphicsAdvancedPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<EnvironmentMapStatus>()
            .init_resource::<EnvironmentMapStableHandles>()
            .init_resource::<EnvironmentMapDebugSphereEntity>()
            .add_systems(PostStartup, (apply_initial_post_fx, sync_environment_map))
            .add_systems(
                Update,
                (
                    enforce_ssao_msaa_exclusion.before(sync_camera_post_fx),
                    sync_camera_post_fx,
                    log_environment_map_load_failures,
                    sync_environment_map_debug_sphere,
                ),
            )
            .add_systems(
                PostUpdate,
                (
                    sync_environment_map,
                    sync_mtoon_ibl_shader_flags,
                    sync_environment_map_debug_sphere_transform,
                ),
            );
    }
}

/// Clears `ssao_enabled` when multisampling is on so we never hit Bevy’s
/// runtime check that rejects SSAO + MSAA together.
fn enforce_ssao_msaa_exclusion(mut settings: ResMut<Settings>) {
    if settings.graphics.msaa_samples >= 2 && settings.graphics.advanced.ssao_enabled {
        settings.graphics.advanced.ssao_enabled = false;
    }
}

fn apply_initial_post_fx(
    settings: Res<Settings>,
    mut commands: Commands,
    cam_q: Query<Entity, With<Camera3d>>,
) {
    for ent in &cam_q {
        apply_post_fx_to_entity(
            &mut commands,
            ent,
            &settings.graphics.advanced,
            settings.graphics.msaa_samples,
        );
    }
}

fn sync_camera_post_fx(
    settings: Res<Settings>,
    mut commands: Commands,
    cam_q: Query<Entity, With<Camera3d>>,
) {
    if !settings.is_changed() {
        return;
    }
    for ent in &cam_q {
        apply_post_fx_to_entity(
            &mut commands,
            ent,
            &settings.graphics.advanced,
            settings.graphics.msaa_samples,
        );
    }
}

fn apply_post_fx_to_entity(
    commands: &mut Commands,
    entity: Entity,
    adv: &GraphicsAdvancedSettings,
    msaa_samples: u32,
) {
    commands
        .entity(entity)
        .insert(parse_tonemap(&adv.tonemapping));

    if adv.bloom.enabled {
        let mut b = Bloom::default();
        b.intensity = adv.bloom.intensity;
        b.low_frequency_boost = adv.bloom.low_frequency_boost;
        b.high_pass_frequency = adv.bloom.high_pass_frequency;
        b.prefilter = BloomPrefilter {
            threshold: adv.bloom.threshold,
            threshold_softness: adv.bloom.threshold_softness,
        };
        b.composite_mode = match adv.bloom.composite_mode.as_str() {
            "additive" | "Additive" => BloomCompositeMode::Additive,
            _ => BloomCompositeMode::EnergyConserving,
        };
        commands.entity(entity).insert(b);
    } else {
        commands.entity(entity).remove::<Bloom>();
    }

    if let Some(preset) = parse_smaa_preset(&adv.smaa_preset) {
        commands.entity(entity).insert(Smaa { preset });
    } else {
        commands.entity(entity).remove::<Smaa>();
    }

    if adv.fxaa_enabled {
        commands.entity(entity).insert(Fxaa::default());
    } else {
        commands.entity(entity).remove::<Fxaa>();
    }

    if adv.auto_exposure {
        commands.entity(entity).insert(AutoExposure::default());
    } else {
        commands.entity(entity).remove::<AutoExposure>();
    }

    if adv.ssao_enabled && msaa_allows_ssao(msaa_samples) {
        commands.entity(entity).insert(ScreenSpaceAmbientOcclusion {
            quality_level: parse_ssao_quality(&adv.ssao_quality),
            constant_object_thickness: adv.ssao_constant_object_thickness.clamp(0.01, 4.0),
            ..default()
        });
    } else {
        commands
            .entity(entity)
            .remove::<ScreenSpaceAmbientOcclusion>();
    }
}

fn parse_tonemap(name: &str) -> Tonemapping {
    match name {
        "None" => Tonemapping::None,
        "Reinhard" => Tonemapping::Reinhard,
        "ReinhardLuminance" => Tonemapping::ReinhardLuminance,
        "AcesFitted" | "ACES" | "Aces" => Tonemapping::AcesFitted,
        "AgX" | "AGX" => Tonemapping::AgX,
        "SomewhatBoringDisplayTransform" | "Somewhat" => {
            Tonemapping::SomewhatBoringDisplayTransform
        }
        "BlenderFilmic" | "Blender" => Tonemapping::BlenderFilmic,
        _ => Tonemapping::TonyMcMapface,
    }
}

fn parse_smaa_preset(name: &str) -> Option<SmaaPreset> {
    match name {
        "Off" | "None" | "off" | "none" | "" => None,
        "Low" | "low" => Some(SmaaPreset::Low),
        "Medium" | "medium" => Some(SmaaPreset::Medium),
        "High" | "high" => Some(SmaaPreset::High),
        "Ultra" | "ultra" => Some(SmaaPreset::Ultra),
        _ => Some(SmaaPreset::Medium),
    }
}

fn parse_ssao_quality(name: &str) -> ScreenSpaceAmbientOcclusionQualityLevel {
    match name.trim() {
        "Low" | "low" => ScreenSpaceAmbientOcclusionQualityLevel::Low,
        "Medium" | "medium" => ScreenSpaceAmbientOcclusionQualityLevel::Medium,
        "Ultra" | "ultra" => ScreenSpaceAmbientOcclusionQualityLevel::Ultra,
        "High" | "high" | _ => ScreenSpaceAmbientOcclusionQualityLevel::High,
    }
}

/// Syncs `EnvironmentMapLight` on the main camera from
/// `Settings::graphics.advanced.environment_map`. Expects
/// `assets/<stem>_diffuse.ktx2` and `assets/<stem>_specular.ktx2` (e.g. stem
/// `maps/` → `maps/_diffuse.ktx2`). Re-runs until both cubemaps finish loading.
#[derive(Default)]
struct EnvMapLogOnce {
    logged_loading: bool,
    logged_active: bool,
    logged_not_cubemap: bool,
}

fn load_state_label(state: LoadState) -> String {
    match state {
        LoadState::NotLoaded => "NotLoaded".to_string(),
        LoadState::Loading => "Loading".to_string(),
        LoadState::Loaded => "Loaded".to_string(),
        LoadState::Failed(err) => format!("Failed({err})"),
    }
}

/// Sparse average luminance of an RGBA16F cubemap (validates map is not empty/black).
fn sample_rgba16f_cubemap_luminance(img: &Image) -> Option<f32> {
    if img.texture_descriptor.format != TextureFormat::Rgba16Float {
        return None;
    }
    let data = img.data.as_ref()?;
    if data.len() < 8 {
        return None;
    }
    let mut sum = 0.0f32;
    let mut count = 0u32;
    let pixel_bytes = 8usize;
    let mut offset = 0usize;
    while offset + pixel_bytes <= data.len() && count < 4096 {
        let r = half::f16::from_le_bytes([data[offset], data[offset + 1]]).to_f32();
        let g = half::f16::from_le_bytes([data[offset + 2], data[offset + 3]]).to_f32();
        let b = half::f16::from_le_bytes([data[offset + 4], data[offset + 5]]).to_f32();
        sum += (r + g + b) / 3.0;
        count += 1;
        offset += pixel_bytes * 97;
    }
    if count == 0 {
        None
    } else {
        Some(sum / count as f32)
    }
}

fn log_environment_map_load_failures(
    mut failures: MessageReader<AssetLoadFailedEvent<Image>>,
) {
    for event in failures.read() {
        warn!(
            "environment map asset failed: id={:?} path={:?} error={:?}",
            event.id, event.path, event.error
        );
    }
}

fn sync_environment_map_debug_sphere(
    settings: Res<Settings>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut sphere_ent: ResMut<EnvironmentMapDebugSphereEntity>,
    existing: Query<Entity, With<EnvironmentMapDebugSphere>>,
) {
    let want = settings.graphics.advanced.environment_map_debug_sphere;
    if !want {
        for e in &existing {
            commands.entity(e).despawn();
        }
        sphere_ent.0 = None;
        return;
    }
    if sphere_ent.0.is_some() || !existing.is_empty() {
        return;
    }
    let mesh = meshes.add(Sphere::new(0.45));
    let mat = materials.add(StandardMaterial {
        base_color: Color::srgb(0.85, 0.85, 0.85),
        perceptual_roughness: 0.35,
        metallic: 0.0,
        ..default()
    });
    let ent = commands
        .spawn((
            EnvironmentMapDebugSphere,
            Mesh3d(mesh),
            MeshMaterial3d(mat),
            Transform::from_xyz(1.35, 1.45, 0.0),
            Name::new("IBL Debug Sphere (StandardMaterial)"),
        ))
        .id();
    sphere_ent.0 = Some(ent);
}

fn image_is_environment_cubemap(img: &Image) -> bool {
    if let Some(view) = &img.texture_view_descriptor {
        matches!(
            view.dimension,
            Some(TextureViewDimension::Cube) | Some(TextureViewDimension::CubeArray)
        )
    } else {
        img.texture_descriptor.dimension == TextureDimension::D2
            && img.texture_descriptor.size.depth_or_array_layers >= 6
    }
}

fn sync_environment_map(
    settings: Res<Settings>,
    asset_server: Res<AssetServer>,
    images: Res<Assets<Image>>,
    mut commands: Commands,
    mut status: ResMut<EnvironmentMapStatus>,
    mut stable: ResMut<EnvironmentMapStableHandles>,
    mut log_once: Local<EnvMapLogOnce>,
    mut cam_q: Query<
        (Entity, Option<Mut<EnvironmentMapLight>>, &GlobalTransform),
        With<Camera3d>,
    >,
) {
    let adv = &settings.graphics.advanced;
    let g = &settings.graphics;
    let stem = adv.environment_map.trim();
    let prev_stem = status.stem.clone();
    if prev_stem != stem {
        stable.diffuse = None;
        stable.specular = None;
        log_once.logged_loading = false;
        log_once.logged_active = false;
        log_once.logged_not_cubemap = false;
    }
    let enabled = adv.environment_map_enabled;
    let nits = adv.environment_nits();
    let yaw = adv.environment_map_rotation_yaw_deg;
    let ibl_active = enabled && !stem.is_empty();

    *status = EnvironmentMapStatus {
        stem: stem.to_string(),
        intensity_nits: nits,
        rotation_yaw_deg: yaw,
        effective_ambient_brightness: g.effective_ambient_brightness(ibl_active),
        ..default()
    };

    for (ent, existing, cam_tf) in &mut cam_q {
        let rotation = adv.environment_rotation(cam_tf.rotation());
        if !enabled || stem.is_empty() {
            if existing.is_some() {
                commands.entity(ent).remove::<EnvironmentMapLight>();
            }
            status.message = if stem.is_empty() {
                "No stem set.".to_string()
            } else {
                "IBL disabled (enable checkbox).".to_string()
            };
            continue;
        }

        let diffuse_path = format!("{stem}_diffuse.ktx2");
        let specular_path = format!("{stem}_specular.ktx2");
        if stable.diffuse.is_none() {
            stable.diffuse = Some(asset_server.load(&diffuse_path));
            stable.specular = Some(asset_server.load(&specular_path));
        }
        let diffuse = stable.diffuse.clone().expect("diffuse handle");
        let specular = stable.specular.clone().expect("specular handle");
        status.diffuse_load_state = load_state_label(asset_server.load_state(&diffuse));
        status.specular_load_state = load_state_label(asset_server.load_state(&specular));
        status.diffuse_loaded = asset_server.is_loaded_with_dependencies(&diffuse);
        status.specular_loaded = asset_server.is_loaded_with_dependencies(&specular);

        if !status.diffuse_loaded || !status.specular_loaded {
            status.message = format!(
                "Loading {diffuse_path} ({}) / {specular_path} ({})…",
                status.diffuse_load_state, status.specular_load_state
            );
            if !log_once.logged_loading {
                log_once.logged_loading = true;
                info!(
                    "environment map: loading {diffuse_path} and {specular_path}"
                );
            }
            continue;
        }

        let Some(diffuse_img) = images.get(&diffuse) else {
            status.message = "Diffuse loaded flag set but Image asset missing.".to_string();
            continue;
        };
        let cubic = image_is_environment_cubemap(diffuse_img);
        status.diffuse_is_cubemap = cubic;
        status.diffuse_format = format!("{:?}", diffuse_img.texture_descriptor.format);
        status.diffuse_data_bytes = diffuse_img.data.as_ref().map(|d| d.len()).unwrap_or(0);
        status.diffuse_sample_luminance =
            sample_rgba16f_cubemap_luminance(diffuse_img).unwrap_or(0.0);
        status.maps_look_valid = cubic
            && status.diffuse_data_bytes > 0
            && status.diffuse_sample_luminance > 0.001;
        if !cubic {
            status.message =
                "Diffuse map loaded but is not a cubemap — re-export as KTX2 cube.".to_string();
            if !log_once.logged_not_cubemap {
                log_once.logged_not_cubemap = true;
                warn!("environment map: {diffuse_path} is not a cubemap texture");
            }
            continue;
        }
        if !status.maps_look_valid {
            status.message = format!(
                "Cubemap OK but looks black/empty on CPU (sample lum {:.4}). Re-export from glTF IBL Sampler.",
                status.diffuse_sample_luminance
            );
            continue;
        }

        if let Some(mut probe) = existing {
            probe.diffuse_map = diffuse.clone();
            probe.specular_map = specular.clone();
            probe.intensity = nits;
            probe.rotation = rotation;
            probe.affects_lightmapped_mesh_diffuse = true;
            status.camera_intensity_nits = probe.intensity;
            status.attached = true;
            status.message = format!(
                "Active — {:.1} nits, lum {:.3}, ambient {:.2}. Orbit behind model; try debug sphere.",
                nits,
                status.diffuse_sample_luminance,
                status.effective_ambient_brightness
            );
            continue;
        }

        commands.entity(ent).insert(EnvironmentMapLight {
            diffuse_map: diffuse,
            specular_map: specular,
            intensity: nits,
            rotation,
            affects_lightmapped_mesh_diffuse: true,
        });
        status.camera_intensity_nits = nits;
        status.attached = true;
        status.message = format!(
            "Attached — {nits:.1} nits. Orbit behind the character to see cubemap light."
        );
        if !log_once.logged_active {
            log_once.logged_active = true;
            info!(
                "environment map active: {diffuse_path} + {specular_path} @ {nits:.1} nits \
                 (sample luminance {:.3})",
                status.diffuse_sample_luminance
            );
        }
    }
}

fn sync_mtoon_ibl_shader_flags(settings: Res<Settings>) {
    let adv = &settings.graphics.advanced;
    bevy_vrm1::vrm::MTOON_IBL_DEBUG_VISUALIZE.store(
        adv.environment_map_debug_visualize,
        std::sync::atomic::Ordering::Relaxed,
    );
    bevy_vrm1::vrm::MTOON_IBL_BODY_GAIN.store(
        adv.environment_map_mtoon_body_gain.clamp(0.5, 16.0).to_bits(),
        std::sync::atomic::Ordering::Relaxed,
    );
}

/// Keeps the PBR debug ball in the camera's forward-right-up corner.
fn sync_environment_map_debug_sphere_transform(
    camera: Query<&GlobalTransform, With<Camera3d>>,
    mut sphere: Query<&mut Transform, With<EnvironmentMapDebugSphere>>,
) {
    let Ok(cam) = camera.single() else {
        return;
    };
    let offset = cam.rotation() * Vec3::new(1.1, 0.55, -1.4);
    for mut tf in &mut sphere {
        tf.translation = cam.translation() + offset;
        tf.rotation = cam.rotation();
    }
}
