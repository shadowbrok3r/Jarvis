//! Tier-2 post-processing (tonemap + bloom + AA + env map + auto-exposure)
//! applied to the main `Camera3d` via `Settings::graphics.advanced`.
//!
//! The plugin idempotently inserts / removes components whenever settings
//! change so the Graphics Advanced window can toggle effects live. HDR is
//! required for bloom / tonemapping to be perceptible, so the plugin
//! respects `Settings::graphics.hdr` and simply no-ops when it's off.

use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::post_process::{
    auto_exposure::AutoExposure,
    bloom::{Bloom, BloomCompositeMode, BloomPrefilter},
};
use bevy::anti_alias::{
    fxaa::Fxaa,
    smaa::{Smaa, SmaaPreset},
};
use bevy::light::EnvironmentMapLight;
use bevy::pbr::{ScreenSpaceAmbientOcclusion, ScreenSpaceAmbientOcclusionQualityLevel};
use bevy::prelude::*;

use jarvis_avatar::config::{msaa_allows_ssao, GraphicsAdvancedSettings, Settings};

pub struct GraphicsAdvancedPlugin;

impl Plugin for GraphicsAdvancedPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(PostStartup, apply_initial_post_fx)
            .add_systems(
                Update,
                (
                    enforce_ssao_msaa_exclusion.before(sync_camera_post_fx),
                    sync_camera_post_fx,
                ),
            )
            .add_systems(Update, sync_environment_map);
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
    commands.entity(entity).insert(parse_tonemap(&adv.tonemapping));

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
        commands.entity(entity).remove::<ScreenSpaceAmbientOcclusion>();
    }
}

fn parse_tonemap(name: &str) -> Tonemapping {
    match name {
        "None" => Tonemapping::None,
        "Reinhard" => Tonemapping::Reinhard,
        "ReinhardLuminance" => Tonemapping::ReinhardLuminance,
        "AcesFitted" | "ACES" | "Aces" => Tonemapping::AcesFitted,
        "AgX" | "AGX" => Tonemapping::AgX,
        "SomewhatBoringDisplayTransform" | "Somewhat" => Tonemapping::SomewhatBoringDisplayTransform,
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
/// `Settings::graphics.advanced.environment_map`. We expect two KTX2 cubemaps
/// side by side named `<stem>_diffuse.ktx2` + `<stem>_specular.ktx2` beneath
/// `assets/`. When the stem is blank, any existing component is removed.
fn sync_environment_map(
    settings: Res<Settings>,
    asset_server: Res<AssetServer>,
    mut commands: Commands,
    cam_q: Query<Entity, With<Camera3d>>,
) {
    if !settings.is_changed() {
        return;
    }
    let adv = &settings.graphics.advanced;
    let stem = adv.environment_map.trim();
    for ent in &cam_q {
        if stem.is_empty() {
            commands.entity(ent).remove::<EnvironmentMapLight>();
            continue;
        }
        let diffuse: Handle<Image> = asset_server.load(format!("{stem}_diffuse.ktx2"));
        let specular: Handle<Image> = asset_server.load(format!("{stem}_specular.ktx2"));
        // Map UI "nits" to Bevy: older `user.toml` used 100–4000 as a blind multiplier.
        let mut nits = adv.environment_intensity.max(0.0);
        if nits > 120.0 {
            nits = nits / 100.0;
        }
        commands.entity(ent).insert(EnvironmentMapLight {
            diffuse_map: diffuse,
            specular_map: specular,
            intensity: nits,
            rotation: Quat::IDENTITY,
            affects_lightmapped_mesh_diffuse: true,
        });
    }
}
