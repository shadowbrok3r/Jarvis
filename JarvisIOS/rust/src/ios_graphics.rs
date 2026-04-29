//! Subset of desktop `[graphics]` mirrored by the hub manifest (`JarvisIosGraphicsLite`).

use bevy::prelude::*;
use serde_json::Value;

/// Lighting + MSAA snapshot for the embedded viewer (from `jarvis-ios.profile.v1` → `graphics`).
#[derive(Resource, Clone, Debug, PartialEq)]
pub struct IosGraphicsSettings {
    pub msaa_samples: u32,
    pub hdr: bool,
    pub exposure_ev100: f32,
    pub ambient_brightness: f32,
    pub ambient_color: [f32; 4],
    pub directional_illuminance: f32,
    pub directional_shadows: bool,
    pub directional_position: Vec3,
    pub show_ground_plane: bool,
    /// Full width/depth of the ground quad in world units (matches desktop `graphics.ground_size`).
    pub ground_size: f32,
    /// Linear RGB for the ground material (matches desktop `graphics.ground_base_color`).
    pub ground_base_color: [f32; 3],
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
    // iOS Metal: `Rgba16Float` (HDR) guarantees MSAA only in {1,2,4}; `Sample8` panics in `create_texture`.
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
