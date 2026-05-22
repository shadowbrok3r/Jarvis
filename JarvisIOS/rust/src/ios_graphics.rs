//! Subset of desktop `[graphics]` + `[light_rig]` mirrored by the hub manifest (`JarvisIosGraphicsLite`).

use bevy::prelude::*;
use serde::Deserialize;
use serde_json::Value;

/// One directional light in the anime rig (matches desktop `LightSpec`).
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct IosLightSpec {
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub direction: [f32; 3],
    pub color: [f32; 3],
    pub illuminance: f32,
    #[serde(default)]
    pub shadows: bool,
}

impl Default for IosLightSpec {
    fn default() -> Self {
        Self {
            enabled: true,
            direction: [-0.6, -1.0, -0.8],
            color: [1.0, 0.96, 0.90],
            illuminance: 9000.0,
            shadows: true,
        }
    }
}

fn default_fill_light() -> IosLightSpec {
    IosLightSpec {
        enabled: true,
        direction: [0.8, -0.4, -0.6],
        color: [0.75, 0.85, 1.0],
        illuminance: 3500.0,
        shadows: false,
    }
}

fn default_rim_light() -> IosLightSpec {
    IosLightSpec {
        enabled: true,
        direction: [-0.25, -0.55, -1.0],
        color: [1.0, 0.88, 0.78],
        illuminance: 7500.0,
        shadows: false,
    }
}

fn default_back_light() -> IosLightSpec {
    IosLightSpec {
        enabled: true,
        direction: [0.0, -0.12, -1.0],
        color: [0.92, 0.94, 1.0],
        illuminance: 6500.0,
        shadows: false,
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct IosLightRigSettings {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub key: IosLightSpec,
    #[serde(default = "default_fill_light")]
    pub fill: IosLightSpec,
    #[serde(default = "default_rim_light")]
    pub rim: IosLightSpec,
    #[serde(default = "default_back_light")]
    pub back: IosLightSpec,
}

impl Default for IosLightRigSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            key: IosLightSpec::default(),
            fill: default_fill_light(),
            rim: default_rim_light(),
            back: default_back_light(),
        }
    }
}

fn default_true() -> bool {
    true
}

/// Lighting + MSAA snapshot for the embedded viewer (from `jarvis-ios.profile.v1` → `graphics`).
#[derive(Resource, Clone, Debug, PartialEq)]
pub struct IosGraphicsSettings {
    pub msaa_samples: u32,
    pub hdr: bool,
    pub exposure_ev100: f32,
    pub ambient_brightness: f32,
    pub ambient_color: [f32; 4],
    /// Legacy single-sun fields (used only when `light_rig.enabled` is false).
    pub directional_illuminance: f32,
    pub directional_shadows: bool,
    pub directional_position: Vec3,
    pub show_ground_plane: bool,
    pub ground_size: f32,
    pub ground_base_color: [f32; 3],
    pub light_rig: IosLightRigSettings,
}

impl Default for IosGraphicsSettings {
    fn default() -> Self {
        Self {
            msaa_samples: 4,
            hdr: true,
            exposure_ev100: 9.7,
            ambient_brightness: 0.04,
            ambient_color: [0.55, 0.58, 0.72, 1.0],
            directional_illuminance: 120_000.0,
            directional_shadows: true,
            directional_position: Vec3::new(4.0, 10.0, 6.0),
            show_ground_plane: true,
            ground_size: 24.0,
            ground_base_color: [0.02, 0.021, 0.025],
            light_rig: IosLightRigSettings::default(),
        }
    }
}

pub fn graphics_from_manifest_value(g: Option<&Value>) -> IosGraphicsSettings {
    let Some(v) = g else {
        return IosGraphicsSettings::default();
    };
    let mut s = IosGraphicsSettings::default();
    if let Some(n) = v.get("msaa_samples").and_then(|x| x.as_u64()) {
        s.msaa_samples = n as u32;
    }
    if let Some(b) = v.get("hdr").and_then(|x| x.as_bool()) {
        s.hdr = b;
    }
    if let Some(x) = v.get("exposure_ev100").and_then(|x| x.as_f64()) {
        s.exposure_ev100 = x as f32;
    }
    if let Some(x) = v.get("ambient_brightness").and_then(|x| x.as_f64()) {
        s.ambient_brightness = x as f32;
    }
    if let Some(Value::Array(arr)) = v.get("ambient_color") {
        if arr.len() == 4 {
            for i in 0..4 {
                s.ambient_color[i] = arr[i].as_f64().unwrap_or(0.0) as f32;
            }
        }
    }
    if let Some(x) = v.get("directional_illuminance").and_then(|x| x.as_f64()) {
        s.directional_illuminance = x as f32;
    }
    if let Some(b) = v.get("directional_shadows").and_then(|x| x.as_bool()) {
        s.directional_shadows = b;
    }
    if let Some(Value::Array(arr)) = v.get("directional_position") {
        if arr.len() == 3 {
            s.directional_position = Vec3::new(
                arr[0].as_f64().unwrap_or(0.0) as f32,
                arr[1].as_f64().unwrap_or(0.0) as f32,
                arr[2].as_f64().unwrap_or(0.0) as f32,
            );
        }
    }
    if let Some(b) = v.get("show_ground_plane").and_then(|x| x.as_bool()) {
        s.show_ground_plane = b;
    }
    if let Some(x) = v.get("ground_size").and_then(|x| x.as_f64()) {
        let g = x as f32;
        if g.is_finite() && g > 0.0 {
            s.ground_size = g;
        }
    }
    if let Some(Value::Array(arr)) = v.get("ground_base_color") {
        if arr.len() == 3 {
            for i in 0..3 {
                s.ground_base_color[i] = arr[i].as_f64().unwrap_or(0.0) as f32;
            }
        }
    }
    if let Some(lr) = v.get("light_rig") {
        if let Ok(rig) = serde_json::from_value::<IosLightRigSettings>(lr.clone()) {
            s.light_rig = rig;
        }
    }
    s.msaa_samples = s.msaa_samples.min(4);
    s
}

/// Map arbitrary sample counts to Bevy's discrete [`Msaa`] variants (avoids `from_samples` panic).
pub fn msaa_for_samples(samples: u32) -> Msaa {
    match samples {
        0 | 1 => Msaa::Off,
        2 => Msaa::Sample2,
        3 | 4 => Msaa::Sample4,
        _ => Msaa::Sample8,
    }
}
