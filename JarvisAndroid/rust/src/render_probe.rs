//! Logs adapter and device limits once at startup.
//!
//! VRMs in this project carry 393–395 joints; a device whose skinning path falls
//! back to the 256-joint uniform buffer renders them wrong rather than failing,
//! so the limit is logged where `adb logcat -s jarvis` can see it.

use bevy::prelude::*;
use bevy::render::renderer::{RenderAdapterInfo, RenderDevice};

pub struct RenderProbePlugin;

impl Plugin for RenderProbePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, log_render_device);
    }
}

fn log_render_device(adapter: Option<Res<RenderAdapterInfo>>, device: Option<Res<RenderDevice>>) {
    if let Some(adapter) = adapter {
        log::info!(
            "adapter: {} backend={:?} type={:?} driver={}",
            adapter.name,
            adapter.backend,
            adapter.device_type,
            adapter.driver,
        );
    }
    if let Some(device) = device {
        let limits = device.limits();
        log::info!(
            "limits: storage_buffers_per_stage={} max_buffer_size={} max_texture_2d={}",
            limits.max_storage_buffers_per_shader_stage,
            limits.max_buffer_size,
            limits.max_texture_dimension_2d,
        );
    }
}
