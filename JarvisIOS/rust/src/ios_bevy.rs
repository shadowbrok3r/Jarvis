//! Bevy 0.18 embedded in a UIKit `UIView` (Metal via wgpu), without `WinitPlugin`.
//! Injects `RawHandleWrapper` before `RenderPlugin` initializes so the swapchain is created.
//! Loads VRM / optional idle VRMA via `bevy_vrm1` (paths align with desktop `config/default.toml`).

use std::ffi::c_void;
use std::f32::consts::FRAC_PI_4;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use bevy::animation::RepeatAnimation;
use bevy::app::AnimationSystems;
use bevy::asset::{AssetMetaCheck, AssetPlugin};
use bevy::camera::{Exposure, PerspectiveProjection, Projection};
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::input::mouse::MouseButton;
use bevy::input::touch::{TouchInput, TouchPhase};
use bevy::mesh::skinning::SkinnedMesh;
use bevy::light::GlobalAmbientLight;
use bevy::prelude::*;
use bevy::render::view::{Hdr, Msaa};
use bevy::render::RenderPlugin;
use bevy::window::{
    CursorOptions, ExitCondition, PrimaryWindow, RawHandleWrapper, RawHandleWrapperHolder,
    WindowPlugin, WindowWrapper,
};
use bevy::winit::WinitPlugin;
use bevy_egui::input::EguiWantsInput;
use bevy_egui::{EguiPlugin, EguiPostUpdateSet, EguiPrimaryContextPass};
use bevy_panorbit_camera::{ActiveCameraData, EguiFocusIncludesHover, PanOrbitCamera, PanOrbitCameraPlugin, TouchControls};
use bevy_vrm1::prelude::*;
use core::ptr::NonNull;

use crate::ios_graphics::{msaa_for_samples, IosGraphicsSettings, IosLightRigSettings, IosLightSpec};
use crate::ios_material_visibility::{
    ios_apply_material_visibility, ios_seed_material_visibility_on_vrm_ready,
    IosMaterialVisibilityJson, IosMaterialVisibilityStore,
};
use crate::ios_mtoon_overrides::{ios_apply_mtoon_overrides_on_vrm_ready, IosMToonOverridesJson};
use crate::ios_profile_manifest::{IosAvatarSettings, IosSpringPresetToml};
use crate::ios_spring_preset::{apply_spring_preset, parse_preset_toml};
use raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, RawWindowHandle, UiKitWindowHandle,
    WindowHandle,
};

// ── Asset root (Swift sets `JARVIS_ASSET_ROOT` to …/assets in the app resource bundle) ─────────

fn ios_asset_file_path() -> String {
    std::env::var("JARVIS_ASSET_ROOT").unwrap_or_else(|_| "assets".to_string())
}

/// Marks the scene root entity for the active VRM (hot-swap / diagnostics).
#[derive(Component, Debug, Clone, Copy, Default)]
pub(crate) struct JarvisIosAvatarRoot;

/// Legacy single sun when `light_rig.enabled` is false.
#[derive(Component)]
struct JarvisIosSun;

#[derive(Component, Copy, Clone, Eq, PartialEq, Debug)]
enum IosLightRigRole {
    Key,
    Fill,
    Rim,
}

#[derive(Component)]
struct JarvisIosGroundPlane;

/// Entity with [`JarvisIosAvatarRoot`] (VRM + optional idle VRMA children).
#[derive(Resource, Default)]
pub(crate) struct IosAvatarRootEntity(pub(crate) Option<Entity>);

/// User toggle for the profile idle VRMA loop (Motion tab).
#[derive(Resource)]
pub struct IosIdlePlaybackState {
    pub user_enabled: bool,
    pub paused_entities: Vec<Entity>,
}

impl Default for IosIdlePlaybackState {
    fn default() -> Self {
        Self {
            user_enabled: true,
            paused_entities: Vec::new(),
        }
    }
}

/// When set, the VRM load is deferred by `remaining` frames to let Bevy's render backend settle.
/// Used for VRMs ≥ [`VRM_WARN_BYTES`] where simultaneous Bevy init + large asset load causes OOM spikes.
#[derive(Resource)]
struct DeferredVrmLoad {
    remaining: u32,
    settings: IosAvatarSettings,
}

/// Live expression preset names + slider weights for the iOS Expressions egui window.
/// Populated by `ios_collect_expression_presets` whenever a VRM finishes initializing.
#[derive(Resource, Default)]
pub(crate) struct IosExpressionsState {
    pub presets: Vec<String>,
    pub weights: std::collections::HashMap<String, f32>,
}

/// Catalog of playable animations discovered under `JARVIS_ASSET_ROOT`. Refreshed by
/// `ios_refresh_animation_catalog` once at startup, then debounced — not every frame.
/// The egui Animations window pulls from this and queues clips through the existing
/// `IosEmbeddedRenderer::queue_*_play` paths (same code Swift uses).
#[derive(Resource, Default)]
pub(crate) struct IosAnimationCatalog {
    /// `models/*.vrma` paths (relative to asset root). Played via `queue_vrma_play`.
    pub vrma_paths: Vec<String>,
    /// `animations/*.json` paths (relative to asset root). Played via `queue_json_anim_play`.
    pub json_paths: Vec<String>,
    /// True once the catalog has been populated at least once.
    pub initialized: bool,
    /// Last manifest revision number when scanned (re-scan when manifest reloads).
    pub last_scan_at: f64,
}

/// Pending animation requests submitted by the egui UI. Drained in the `Last` schedule by
/// `ios_drain_egui_anim_requests`, which forwards them through the same code paths that
/// Swift uses (`flush_queued_vrma_requests` / `flush_queued_json_anim_requests`).
#[derive(Resource, Default)]
pub(crate) struct IosEguiAnimRequests {
    pub vrma: Vec<(String, bool)>,
    pub json: Vec<(String, bool)>,
}

/// Passed into the app before `DefaultPlugins`; consumed by [`IosEmbedRawHandlesPlugin`].
///
/// Manual [`Resource`] impl avoids `#[derive(Resource)]` needing a direct `bevy_ecs` crate path
/// when Bevy is built with `default-features = false` for the iOS staticlib.
struct PendingIosSurface {
    view: NonNull<c_void>,
    width_px: u32,
    height_px: u32,
    scale_factor: f32,
}

// UIKit view pointer: only touched from the main thread via Swift `CADisplayLink` + FFI.
unsafe impl Send for PendingIosSurface {}
unsafe impl Sync for PendingIosSurface {}

impl Resource for PendingIosSurface {}

/// UIKit view pointer as a `HasWindowHandle` source for `RawHandleWrapper`.
#[derive(Clone)]
struct IosUiViewHost(NonNull<c_void>);

unsafe impl Send for IosUiViewHost {}
unsafe impl Sync for IosUiViewHost {}

impl HasWindowHandle for IosUiViewHost {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let ui = UiKitWindowHandle::new(self.0);
        let raw = RawWindowHandle::UiKit(ui);
        Ok(unsafe { WindowHandle::borrow_raw(raw) })
    }
}

impl HasDisplayHandle for IosUiViewHost {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        Ok(DisplayHandle::uikit())
    }
}

struct IosEmbedRawHandlesPlugin;

impl Plugin for IosEmbedRawHandlesPlugin {
    fn build(&self, app: &mut App) {
        let PendingIosSurface {
            view,
            width_px,
            height_px,
            scale_factor,
        } = app
            .world_mut()
            .remove_resource::<PendingIosSurface>()
            .expect("PendingIosSurface must be inserted before DefaultPlugins");

        let host = IosUiViewHost(view);
        let wrapped = WindowWrapper::new(host);
        let handle = RawHandleWrapper::new(&wrapped).expect("RawHandleWrapper::new for UIKit view");

        let entity = {
            let world = app.world_mut();
            let mut q = world.query_filtered::<(Entity, &mut Window, &RawHandleWrapperHolder), With<PrimaryWindow>>();
            let Ok((entity, mut window, holder)) = q.single_mut(world) else {
                panic!("JarvisIOS Bevy: missing PrimaryWindow entity");
            };

            window
                .resolution
                .set_physical_resolution(width_px.max(1), height_px.max(1));
            window.resolution.set_scale_factor(scale_factor);

            *holder.0.lock().expect("RawHandleWrapperHolder mutex") = Some(handle.clone());
            entity
        };

        app.world_mut().entity_mut(entity).insert(handle);
    }
}

// ── VRM scene (adapted from desktop `plugins/avatar.rs`) ───────────────────────────────────────

/// Log key Bevy / wgpu render-device limits once after the renderer has been initialized.
///
/// We need to know definitively whether iOS Metal exposes storage buffers (which would let Bevy
/// use the unbounded `array<mat4x4<f32>>` storage-buffer skinning path) or whether it's falling
/// back to fixed-size 256-joint uniform buffers. The crash on frame ~12 happens the first time
/// a skinned mesh is rendered; if `max_storage_buffers_per_shader_stage = 0` then Bevy is using
/// the uniform path and the 256-joint hard limit applies — that's a likely root cause for the
/// 393-395-joint VRMs we're loading.
fn log_render_device_limits(
    mut done: Local<bool>,
    render_device: Option<Res<bevy::render::renderer::RenderDevice>>,
    render_adapter_info: Option<Res<bevy::render::renderer::RenderAdapterInfo>>,
) {
    if *done {
        return;
    }
    let Some(rd) = render_device else { return };
    let limits = rd.limits();
    let features = rd.features();

    if let Some(info) = render_adapter_info.as_deref() {
        crate::jarvis_ios_line!(
            "[JarvisIOS] wgpu adapter: name='{}' backend={:?} device_type={:?} driver='{}'",
            info.name, info.backend, info.device_type, info.driver,
        );
    }
    crate::jarvis_ios_line!(
        "[JarvisIOS] wgpu limits: max_storage_buffers_per_shader_stage={} \
         max_uniform_buffers_per_shader_stage={} \
         max_uniform_buffer_binding_size={} \
         max_buffer_size={} \
         max_bind_groups={}",
        limits.max_storage_buffers_per_shader_stage,
        limits.max_uniform_buffers_per_shader_stage,
        limits.max_uniform_buffer_binding_size,
        limits.max_buffer_size,
        limits.max_bind_groups,
    );
    crate::jarvis_ios_line!(
        "[JarvisIOS] wgpu features: {:?}",
        features,
    );

    // Decide which skinning path Bevy will pick — same logic as bevy_pbr::render::skin::skins_use_uniform_buffers
    let skins_uniform = limits.max_storage_buffers_per_shader_stage == 0;
    crate::jarvis_ios_line!(
        "[JarvisIOS] skinning path: {} {}",
        if skins_uniform { "UNIFORM_BUFFERS" } else { "STORAGE_BUFFERS" },
        if skins_uniform {
            "(⚠ HARD 256-joint limit; VRMs with >256 joints WILL CRASH on Metal)"
        } else {
            "(unbounded array; VRMs with any joint count should work)"
        },
    );

    *done = true;
}

fn ios_light_vis(enabled: bool) -> Visibility {
    if enabled {
        Visibility::Visible
    } else {
        Visibility::Hidden
    }
}

fn spawn_ios_light_one(commands: &mut Commands, role: IosLightRigRole, spec: &IosLightSpec) {
    let direction = Vec3::from_array(spec.direction).normalize_or_zero();
    let transform = if direction.length_squared() > 0.0 {
        Transform::IDENTITY.looking_to(direction, Vec3::Y)
    } else {
        Transform::IDENTITY
    };
    commands.spawn((
        DirectionalLight {
            color: Color::linear_rgb(spec.color[0], spec.color[1], spec.color[2]),
            illuminance: spec.illuminance,
            shadows_enabled: spec.shadows,
            ..default()
        },
        transform,
        ios_light_vis(spec.enabled),
        role,
    ));
}

fn spawn_ios_lights(commands: &mut Commands, graphics: &IosGraphicsSettings, look_at: Vec3) {
    if graphics.light_rig.enabled {
        let rig = &graphics.light_rig;
        spawn_ios_light_one(commands, IosLightRigRole::Key, &rig.key);
        spawn_ios_light_one(commands, IosLightRigRole::Fill, &rig.fill);
        spawn_ios_light_one(commands, IosLightRigRole::Rim, &rig.rim);
        crate::jarvis_ios_line!("[JarvisIOS] spawned 3-light rig (key/fill/rim)");
    } else {
        commands.spawn((
            JarvisIosSun,
            DirectionalLight {
                illuminance: graphics.directional_illuminance,
                shadows_enabled: graphics.directional_shadows,
                ..default()
            },
            Transform::from_translation(graphics.directional_position).looking_at(look_at, Vec3::Y),
        ));
        crate::jarvis_ios_line!("[JarvisIOS] spawned legacy single sun (light_rig disabled)");
    }
}

fn sync_ios_light_rig(world: &mut World, rig: &IosLightRigSettings) {
    let mut q = world.query::<(
        &IosLightRigRole,
        &mut DirectionalLight,
        &mut Transform,
        &mut Visibility,
    )>();
    for (role, mut light, mut tf, mut vis) in q.iter_mut(world) {
        let spec = match role {
            IosLightRigRole::Key => &rig.key,
            IosLightRigRole::Fill => &rig.fill,
            IosLightRigRole::Rim => &rig.rim,
        };
        let effective = rig.enabled && spec.enabled;
        *vis = ios_light_vis(effective);
        light.color = Color::linear_rgb(spec.color[0], spec.color[1], spec.color[2]);
        light.illuminance = spec.illuminance;
        light.shadows_enabled = spec.shadows;
        let direction = Vec3::from_array(spec.direction).normalize_or_zero();
        if direction.length_squared() > 0.0 {
            *tf = Transform::IDENTITY.looking_to(direction, Vec3::Y);
        }
    }
}

fn spawn_ios_viewport(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    settings: Res<IosAvatarSettings>,
    mut graphics: ResMut<IosGraphicsSettings>,
) {
    // Reduce MSAA for large VRMs: decoded textures already push memory pressure hard.
    // An MSAA=4 HDR (Rgba16Float) buffer at iPhone 14 resolution adds ~86 MB; dropping to 1
    // recovers that headroom at the cost of slightly rougher edges.
    let asset_root = ios_asset_file_path();
    let vrm_disk = Path::new(&asset_root).join(&settings.model_path);
    if let Ok(meta) = std::fs::metadata(&vrm_disk) {
        if meta.len() >= VRM_WARN_BYTES && graphics.msaa_samples > 1 {
            graphics.msaa_samples = 1;
            crate::jarvis_ios_line!(
                "[JarvisIOS] Large VRM ({:.1} MB): forcing MSAA off to reclaim ~86 MB MSAA buffer",
                meta.len() as f64 / (1024.0 * 1024.0)
            );
        }
    }
    let focus = settings.world_position;
    let look_at = focus + Vec3::Y * 0.5;
    let [ar, ag, ab, aa] = graphics.ambient_color;
    commands.insert_resource(GlobalAmbientLight {
        color: Color::linear_rgba(ar, ag, ab, aa),
        brightness: graphics.ambient_brightness,
        affects_lightmapped_meshes: true,
    });
    spawn_ios_lights(&mut commands, &graphics, look_at);
    let half = (graphics.ground_size.max(0.02) * 0.5).min(512.0);
    let gc = graphics.ground_base_color;
    let ground_color = Color::linear_rgb(gc[0], gc[1], gc[2]);
    let ground_vis = if graphics.show_ground_plane {
        Visibility::Visible
    } else {
        Visibility::Hidden
    };
    commands.spawn((
        JarvisIosGroundPlane,
        Mesh3d(meshes.add(Mesh::from(Plane3d::new(Vec3::Y, Vec2::splat(half))))),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: ground_color,
            perceptual_roughness: 0.85,
            ..default()
        })),
        Transform::from_xyz(0.0, 0.0, 0.0),
        ground_vis,
    ));

    // Match desktop `OrbitCameraPlugin`: PanOrbit + touch (1-finger orbit, 2-finger pan, pinch zoom).
    // `bevy_panorbit_camera` reads `Touches`; we inject `TouchInput` from UIKit via `jarvis_renderer_touch`.
    let mut orbit = PanOrbitCamera::default();
    orbit.focus = focus;
    orbit.target_focus = focus;
    orbit.target_radius = 3.35;
    orbit.radius = Some(3.35);
    orbit.zoom_lower_limit = 0.35;
    orbit.zoom_upper_limit = Some(96.0);
    orbit.touch_enabled = true;
    orbit.touch_controls = TouchControls::OneFingerOrbit;
    // Prevent camera flipping upside-down (reads as the avatar inverting on screen).
    orbit.pitch_lower_limit = Some(-std::f32::consts::FRAC_PI_2 + 0.12);
    orbit.pitch_upper_limit = Some(std::f32::consts::FRAC_PI_2 - 0.12);
    orbit.button_orbit = MouseButton::Left;
    orbit.button_pan = MouseButton::Middle;
    orbit.button_zoom = None;
    let eye = focus + Vec3::new(0.0, 0.25, 3.35);

    let mut cam = commands.spawn((
        Camera3d::default(),
        msaa_for_samples(graphics.msaa_samples),
        Projection::Perspective(PerspectiveProjection {
            fov: FRAC_PI_4,
            near: 0.08,
            far: 200.0,
            ..default()
        }),
        Transform::from_translation(eye).looking_at(focus, Vec3::Y),
        orbit,
        // Match desktop `orbit_camera` + avoid default `TonyMcMapface` (LUT-heavy) on embedded Metal.
        Exposure {
            ev100: graphics.exposure_ev100,
        },
        Tonemapping::AcesFitted,
    ));
    if graphics.hdr {
        cam.insert(Hdr);
    }
}

/// VRM 1.0 spec-defined preset expression names (universal across all VRM avatars).
/// These are the only names defined in the VRMC_vrm specification; everything else
/// loaded at runtime is a custom/model-specific expression.
pub(crate) const VRM1_SPEC_PRESET_NAMES: &[&str] = &[
    "aa", "ih", "ou", "ee", "oh",
    "blink", "blinkLeft", "blinkRight",
    "lookUp", "lookDown", "lookLeft", "lookRight",
    "happy", "angry", "sad", "relaxed", "surprised", "neutral",
];

/// Returns true if `name` is one of the 18 names defined in the VRM 1.0 spec.
pub(crate) fn is_vrm1_spec_preset(name: &str) -> bool {
    VRM1_SPEC_PRESET_NAMES.contains(&name)
}

/// Log a diagnostic advisory when the on-disk VRM exceeds this size.
/// Textures in a VRM are stored as JPEG/PNG and decoded to raw RGBA on load, so the
/// in-RAM footprint can be several times the file size.  MSAA is already forced off for
/// VRMs at this threshold (see `spawn_ios_viewport`), which reclaims ~86 MB.
const VRM_WARN_BYTES: u64 = 150 * 1024 * 1024; // 150 MB
/// Hard limit: refuse to load rather than OOM-kill the process.
const VRM_HARD_LIMIT_BYTES: u64 = 260 * 1024 * 1024; // 260 MB

/// Resolve the on-disk path for the model. If `<basename>.ios.vrm` exists in the same
/// directory as the requested model, return that path instead. Returns the *relative-to-asset-root*
/// path (suitable for both `AssetServer::load` and `Path::join(asset_root, ...)`).
fn resolve_ios_variant_path(asset_root: &str, model_path: &str) -> String {
    // Trim a trailing `.vrm` (case-insensitive) and check for `<base>.ios.vrm` alongside it.
    let lower = model_path.to_ascii_lowercase();
    let base: &str = if let Some(stripped) = lower.strip_suffix(".vrm") {
        &model_path[..stripped.len()]
    } else {
        return model_path.to_string();
    };
    let candidate = format!("{base}.ios.vrm");
    let candidate_disk = Path::new(asset_root).join(&candidate);
    if candidate_disk.is_file() {
        candidate
    } else {
        model_path.to_string()
    }
}

/// Spawns [`JarvisIosAvatarRoot`] + [`VrmHandle`], optional idle VRMA; returns the root [`Entity`].
fn spawn_jarvis_ios_vrm_root(commands: &mut Commands, asset_server: &AssetServer, settings: &IosAvatarSettings) -> Entity {
    let asset_root = ios_asset_file_path();

    // Prefer the `<basename>.ios.vrm` variant if it exists alongside the source. This lets the
    // desktop pre-process step (scripts/compress_vrm_textures.py --ios-variant) ship a slim
    // texture-downscaled copy without touching the original file. Source `3.vrm` decodes to
    // ~650 MB of GPU RGBA on this device; the iOS variant fits in ~50 MB.
    let resolved_model_path = resolve_ios_variant_path(&asset_root, &settings.model_path);
    let vrm_disk = Path::new(&asset_root).join(&resolved_model_path);
    let vrm_bytes = std::fs::metadata(&vrm_disk).map(|m| m.len()).unwrap_or(0);
    if resolved_model_path != settings.model_path {
        crate::jarvis_ios_line!(
            "[JarvisIOS] using iOS-variant VRM '{}' (instead of '{}')",
            resolved_model_path, settings.model_path,
        );
    }
    crate::jarvis_ios_line!(
        "[JarvisIOS] spawn_jarvis_ios_vrm_root model_path={} size={:.1}MB exists_on_disk={}",
        resolved_model_path,
        vrm_bytes as f64 / (1024.0 * 1024.0),
        vrm_disk.is_file()
    );
    if vrm_bytes >= VRM_WARN_BYTES {
        crate::jarvis_ios_line!(
            "[JarvisIOS] Warning: VRM is large ({:.1} MB) — MSAA disabled; may cause memory \
             pressure on older devices with less RAM",
            vrm_bytes as f64 / (1024.0 * 1024.0)
        );
    }
    if vrm_bytes >= VRM_HARD_LIMIT_BYTES {
        crate::jarvis_ios_line!(
            "[JarvisIOS] VRM too large for iOS ({:.1} MB \u{2265} {} MB hard limit) — skipping load \
             to prevent OOM crash.  Run scripts/compress_vrm_textures.py to reduce it.",
            vrm_bytes as f64 / (1024.0 * 1024.0),
            VRM_HARD_LIMIT_BYTES / (1024 * 1024)
        );
        return commands.spawn((
            JarvisIosAvatarRoot,
            Transform::default(),
            GlobalTransform::default(),
        )).id();
    }

    if !settings.idle_vrma_path.trim().is_empty() {
        let vrma_disk = Path::new(&asset_root).join(settings.idle_vrma_path.trim());
        crate::jarvis_ios_line!(
            "[JarvisIOS] spawn_jarvis_ios_vrm_root idle_vrma_path={} exists_on_disk={}",
            settings.idle_vrma_path,
            vrma_disk.is_file()
        );
    }

    let pos = settings.world_position;
    let scale = settings.uniform_scale.max(0.001);
    let mut vrm = commands.spawn((
        JarvisIosAvatarRoot,
        Transform {
            translation: pos,
            scale: Vec3::splat(scale),
            ..default()
        },
        GlobalTransform::default(),
        VrmHandle(asset_server.load(resolved_model_path.clone())),
    ));
    crate::jarvis_ios_line!(
        "[JarvisIOS] spawn_jarvis_ios_vrm_root queued VrmHandle for {}",
        resolved_model_path,
    );

    if !settings.idle_vrma_path.trim().is_empty() {
        let path = settings.idle_vrma_path.clone();
        vrm.with_children(|parent| {
            parent
                .spawn(VrmaHandle(asset_server.load(path)))
                .observe(play_idle_when_vrma_loaded);
        });
    }
    vrm.id()
}

fn spawn_ios_avatar(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    settings: Res<IosAvatarSettings>,
) {
    commands.insert_resource(ClearColor(settings.background_color));

    let asset_root = ios_asset_file_path();
    let vrm_disk = Path::new(&asset_root).join(&settings.model_path);
    let vrm_bytes = std::fs::metadata(&vrm_disk).map(|m| m.len()).unwrap_or(0);

    // Defer large VRM loads so Bevy's render backend (wgpu + Metal) fully
    // initialises before the memory-intensive asset load begins.  On an iPhone
    // with limited RAM the simultaneous Bevy startup + large VRM decompression
    // spikes hard enough to trigger the jetsam OOM killer.
    if vrm_bytes >= VRM_WARN_BYTES {
        crate::jarvis_ios_line!(
            "[JarvisIOS] Large VRM ({:.1} MB): deferring load by 8 frames to let renderer settle",
            vrm_bytes as f64 / (1024.0 * 1024.0)
        );
        commands.insert_resource(DeferredVrmLoad {
            remaining: 8,
            settings: settings.clone(),
        });
        commands.insert_resource(IosAvatarRootEntity(None));
        return;
    }
    let id = spawn_jarvis_ios_vrm_root(&mut commands, &asset_server, &settings);
    commands.insert_resource(IosAvatarRootEntity(Some(id)));
}

/// Counts down `DeferredVrmLoad` each frame and spawns the VRM once the counter reaches zero.
fn handle_deferred_vrm_load(
    mut commands: Commands,
    deferred: Option<ResMut<DeferredVrmLoad>>,
    asset_server: Res<AssetServer>,
) {
    let Some(mut d) = deferred else { return; };
    if d.remaining > 0 {
        d.remaining -= 1;
        return;
    }
    let id = spawn_jarvis_ios_vrm_root(&mut commands, &asset_server, &d.settings);
    commands.insert_resource(IosAvatarRootEntity(Some(id)));
    commands.remove_resource::<DeferredVrmLoad>();
    crate::jarvis_ios_line!("[JarvisIOS] deferred VRM load: spawned after render backend settle");
}

/// Populate [`IosExpressionsState`] whenever a new VRM finishes loading.
fn ios_collect_expression_presets(
    mut expr_state: ResMut<IosExpressionsState>,
    vrm_q: Query<&ExpressionEntityMap, (With<Vrm>, Added<Initialized>)>,
) {
    let Ok(map) = vrm_q.single() else { return; };
    let mut names: Vec<String> = map.keys().map(|k| k.0.clone()).collect();
    names.sort();
    names.dedup();
    let old_weights = expr_state.weights.clone();
    expr_state.weights = names
        .iter()
        .map(|n| (n.clone(), old_weights.get(n).copied().unwrap_or(0.0)))
        .collect();
    expr_state.presets = names;
    let spec_count = expr_state.presets.iter().filter(|n| is_vrm1_spec_preset(n)).count();
    crate::jarvis_ios_line!(
        "[JarvisIOS] expressions: collected {} total ({} spec preset + {} custom) from VRM",
        expr_state.presets.len(),
        spec_count,
        expr_state.presets.len() - spec_count,
    );
}

/// Walk the asset root for `.vrma` and `animations/*.json` files exactly once at startup
/// (and any time the catalog is explicitly re-armed). Filesystem walks are *not* free on
/// iOS — we do this on a `Local<bool>` flag so it never repeats per-frame after init.
fn ios_refresh_animation_catalog(
    mut catalog: ResMut<IosAnimationCatalog>,
    time: Res<Time>,
) {
    if catalog.initialized {
        return;
    }
    catalog.initialized = true;
    catalog.last_scan_at = time.elapsed_secs_f64();

    let root = ios_asset_file_path();
    let root_p = std::path::PathBuf::from(&root);

    let mut vrma: Vec<String> = Vec::new();
    let mut json: Vec<String> = Vec::new();

    // Walk a small set of well-known dirs (don't recurse the whole asset bundle).
    for dir in ["models", "animations", "vrma"] {
        let abs = root_p.join(dir);
        let Ok(read) = std::fs::read_dir(&abs) else { continue };
        for entry in read.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
            let lower = name.to_ascii_lowercase();
            if !path.is_file() {
                continue;
            }
            if lower.ends_with(".vrma") {
                vrma.push(format!("{dir}/{name}"));
            } else if lower.ends_with(".json") && dir != "models" {
                json.push(format!("{dir}/{name}"));
            }
        }
    }

    vrma.sort();
    vrma.dedup();
    json.sort();
    json.dedup();

    crate::jarvis_ios_line!(
        "[JarvisIOS] anim catalog: scanned root={} vrma={} json={}",
        root,
        vrma.len(),
        json.len(),
    );

    catalog.vrma_paths = vrma;
    catalog.json_paths = json;
}

/// Apply a single JSON pose-clip request. Mirrors `IosEmbeddedRenderer::flush_queued_json_anim_requests`
/// but as a free function so the egui drain system can reuse it without borrowing the renderer.
pub(crate) fn ios_apply_json_anim_request(world: &mut World, path: String, loop_forever: bool) {
    let old_stopped = world
        .resource::<crate::ios_anim_json::IosJsonAnimPlayback>()
        .supersede_stopped_idle_snapshot();
    let mut clip = crate::ios_anim_json::try_build_clip(&path, world, Some(loop_forever));
    if !old_stopped.is_empty() {
        crate::ios_anim_json::resume_idle_vrmas_on_world(world, &old_stopped);
        world.flush();
    }
    if let Some(ref mut c) = clip {
        if let Some(root) = world.resource::<IosAvatarRootEntity>().0 {
            let settings = world.resource::<IosAvatarSettings>().clone();
            c.stopped_idle_vrma =
                crate::ios_anim_json::pause_matching_idle_vrma(world, root, &settings);
            if !c.stopped_idle_vrma.is_empty() {
                world.flush();
            }
        }
    }
    world
        .resource_mut::<crate::ios_anim_json::IosJsonAnimPlayback>()
        .replace_with_clip(clip);
}

/// Apply a batch of VRMA play requests. Mirrors `IosEmbeddedRenderer::flush_queued_vrma_requests`
/// but as a free function so egui can submit clips without bouncing through the FFI mutex.
pub(crate) fn ios_apply_vrma_requests(world: &mut World, requests: Vec<(String, bool)>) {
    let Some(root) = world.resource::<IosAvatarRootEntity>().0 else {
        crate::jarvis_ios_line!("[JarvisIOS] queue_vrma: no avatar root (reload profile first)");
        return;
    };
    let asset_server = world.resource::<AssetServer>().clone();
    for (path, loop_forever) in requests {
        if !is_safe_asset_rel(&path) {
            crate::jarvis_ios_line!("[JarvisIOS] queue_vrma: rejected unsafe path {path:?}");
            continue;
        }
        if loop_forever {
            world.commands().entity(root).with_children(|parent| {
                parent
                    .spawn(VrmaHandle(asset_server.load(path.clone())))
                    .observe(observe_vrma_play_forever);
            });
        } else {
            world.commands().entity(root).with_children(|parent| {
                parent
                    .spawn(VrmaHandle(asset_server.load(path.clone())))
                    .observe(observe_vrma_play_once);
            });
        }
        crate::jarvis_ios_line!("[JarvisIOS] queue_vrma: spawned {path} loop={loop_forever}");
    }
    world.flush();
}

/// Drain `IosEguiAnimRequests` once per frame and forward to the same code path Swift uses.
/// Runs in `Last` so it executes after the egui systems have populated the resource for this tick.
fn ios_drain_egui_anim_requests(world: &mut World) {
    let (vrma, json) = {
        let mut req = world.resource_mut::<IosEguiAnimRequests>();
        (core::mem::take(&mut req.vrma), core::mem::take(&mut req.json))
    };
    if !vrma.is_empty() {
        ios_apply_vrma_requests(world, vrma);
    }
    if let Some(path) = json.into_iter().last() {
        ios_apply_json_anim_request(world, path.0, path.1);
    }
}

fn play_idle_when_vrma_loaded(
    trigger: On<LoadedVrma>,
    mut commands: Commands,
    idle: Res<IosIdlePlaybackState>,
) {
    if !idle.user_enabled {
        crate::jarvis_ios_line!("[JarvisIOS] idle VRMA loaded — playback disabled by user");
        return;
    }
    commands.trigger(PlayVrma {
        repeat: RepeatAnimation::Forever,
        transition_duration: Duration::ZERO,
        vrma: trigger.vrma,
        reset_spring_bones: false,
    });
}

pub fn ios_set_idle_animation_enabled(world: &mut World, enabled: bool) {
    let was = world.resource::<IosIdlePlaybackState>().user_enabled;
    {
        let mut idle = world.resource_mut::<IosIdlePlaybackState>();
        idle.user_enabled = enabled;
    }
    if was == enabled {
        return;
    }

    let Some(root) = world.resource::<IosAvatarRootEntity>().0 else {
        crate::jarvis_ios_line!("[JarvisIOS] idle playback: no avatar root yet (enabled={enabled})");
        return;
    };
    let settings = world.resource::<IosAvatarSettings>().clone();

    if enabled {
        let paused = world
            .resource::<IosIdlePlaybackState>()
            .paused_entities
            .clone();
        if !paused.is_empty() {
            crate::ios_anim_json::resume_idle_vrmas_on_world(world, &paused);
        } else {
            crate::ios_anim_json::start_matching_idle_vrma(world, root, &settings);
        }
        world
            .resource_mut::<IosIdlePlaybackState>()
            .paused_entities
            .clear();
        world.flush();
        crate::jarvis_ios_line!("[JarvisIOS] idle playback: enabled");
    } else {
        let stopped =
            crate::ios_anim_json::pause_matching_idle_vrma(world, root, &settings);
        world
            .resource_mut::<IosIdlePlaybackState>()
            .paused_entities = stopped.clone();
        if let Some(root) = world.resource::<IosAvatarRootEntity>().0 {
            bevy_vrm1::prelude::reset_spring_velocities_recursive_world(world, root);
        }
        world.flush();
        crate::jarvis_ios_line!(
            "[JarvisIOS] idle playback: paused {} VRMA target(s)",
            stopped.len()
        );
    }
}

fn observe_vrma_play_forever(trigger: On<LoadedVrma>, mut commands: Commands) {
    commands.trigger(PlayVrma {
        repeat: RepeatAnimation::Forever,
        transition_duration: Duration::from_millis(300),
        vrma: trigger.vrma,
        reset_spring_bones: true,
    });
}

fn observe_vrma_play_once(trigger: On<LoadedVrma>, mut commands: Commands) {
    commands.trigger(PlayVrma {
        repeat: RepeatAnimation::Never,
        transition_duration: Duration::from_millis(300),
        vrma: trigger.vrma,
        reset_spring_bones: false,
    });
}

pub(crate) fn is_safe_asset_rel(rel: &str) -> bool {
    !rel.is_empty() && !rel.starts_with('/') && !rel.contains("..")
}

fn apply_scene_graphics_from_settings(world: &mut World, g: &IosGraphicsSettings, focus: Vec3) {
    let look_at = focus + Vec3::Y * 0.5;
    let [ar, ag, ab, aa] = g.ambient_color;
    *world.resource_mut::<GlobalAmbientLight>() = GlobalAmbientLight {
        color: Color::linear_rgba(ar, ag, ab, aa),
        brightness: g.ambient_brightness,
        affects_lightmapped_meshes: true,
    };
    if g.light_rig.enabled {
        sync_ios_light_rig(world, &g.light_rig);
    } else {
        let mut sun_q =
            world.query_filtered::<(&mut DirectionalLight, &mut Transform), With<JarvisIosSun>>();
        for (mut dl, mut tf) in sun_q.iter_mut(world) {
            dl.illuminance = g.directional_illuminance;
            dl.shadows_enabled = g.directional_shadows;
            *tf = Transform::from_translation(g.directional_position).looking_at(look_at, Vec3::Y);
        }
    }
    let cam_entities: Vec<Entity> = world
        .query_filtered::<Entity, With<Camera3d>>()
        .iter(world)
        .collect();
    for e in cam_entities {
        let mut ew = world.entity_mut(e);
        if let Some(mut m) = ew.get_mut::<Msaa>() {
            *m = msaa_for_samples(g.msaa_samples);
        }
        if let Some(mut exp) = ew.get_mut::<Exposure>() {
            exp.ev100 = g.exposure_ev100;
        }
        if g.hdr {
            ew.insert(Hdr);
        } else {
            ew.remove::<Hdr>();
        }
    }
    let half = (g.ground_size.max(0.02) * 0.5).min(512.0);
    let gc = g.ground_base_color;
    let ground_color = Color::linear_rgb(gc[0], gc[1], gc[2]);
    let ground_mesh_handles: Vec<Handle<Mesh>> = world
        .query_filtered::<&Mesh3d, With<JarvisIosGroundPlane>>()
        .iter(world)
        .map(|m| m.0.clone())
        .collect();
    {
        let mut meshes = world.resource_mut::<Assets<Mesh>>();
        for h in &ground_mesh_handles {
            if let Some(m) = meshes.get_mut(h) {
                *m = Mesh::from(Plane3d::new(Vec3::Y, Vec2::splat(half)));
            }
        }
    }
    for mut vis in world
        .query_filtered::<&mut Visibility, With<JarvisIosGroundPlane>>()
        .iter_mut(world)
    {
        *vis = if g.show_ground_plane {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }
    let handles: Vec<Handle<StandardMaterial>> = world
        .query_filtered::<&MeshMaterial3d<StandardMaterial>, With<JarvisIosGroundPlane>>()
        .iter(world)
        .map(|m| m.0.clone())
        .collect();
    let mut mats = world.resource_mut::<Assets<StandardMaterial>>();
    for h in handles {
        if let Some(m) = mats.get_mut(&h) {
            m.base_color = ground_color;
        }
    }
}

fn update_panorbit_camera_focus(world: &mut World, focus: Vec3) {
    let mut q = world.query_filtered::<&mut PanOrbitCamera, With<Camera3d>>();
    for mut orbit in q.iter_mut(world) {
        orbit.focus = focus;
        orbit.target_focus = focus;
    }
}

fn ios_apply_spring_preset_on_vrm_ready(
    preset: Res<IosSpringPresetToml>,
    vrm_ready: Query<(), (With<Vrm>, Added<Initialized>)>,
    mut springs: Query<(Entity, Option<&Name>, &mut SpringJointProps)>,
    mut colliders: Query<(Entity, Option<&Name>, &mut ColliderShape)>,
    mut baselines: ResMut<crate::ios_device_motion::IosSpringMotionBaselines>,
) {
    if preset.0.is_none() || vrm_ready.is_empty() {
        return;
    }
    let Some(raw) = preset.0.as_deref() else {
        return;
    };
    let Ok(p) = parse_preset_toml(raw) else {
        crate::jarvis_ios_line!("[JarvisIOS] spring preset: TOML parse failed");
        return;
    };
    if p.preset_version != crate::ios_spring_preset::PRESET_FORMAT_VERSION {
        crate::jarvis_ios_line!(
            "[JarvisIOS] spring preset: unexpected preset_version {} (expected {})",
            p.preset_version,
            crate::ios_spring_preset::PRESET_FORMAT_VERSION
        );
    }
    let (jh, jm, ch, cm) = apply_spring_preset(&p, &mut springs, &mut colliders);
    baselines.capture_from_springs(springs.iter().map(|(e, _, p)| {
        (
            e,
            crate::ios_device_motion::SpringMotionBaseline {
                gravity_dir: p.gravity_dir,
                gravity_power: p.gravity_power,
                drag_force: p.drag_force,
            },
        )
    }));
    crate::jarvis_ios_line!(
        "[JarvisIOS] spring preset applied: joints {}/{} colliders {}/{}",
        jh,
        jh + jm,
        ch,
        ch + cm
    );
}

fn lock_hips_root_motion(
    settings: Res<IosAvatarSettings>,
    mut hips_q: Query<(&mut Transform, &RestTransform), With<Hips>>,
) {
    if !settings.lock_root_xz && !settings.lock_root_y {
        return;
    }
    for (mut tf, rest) in &mut hips_q {
        let r = rest.0.translation;
        if settings.lock_root_xz {
            tf.translation.x = r.x;
            tf.translation.z = r.z;
        }
        if settings.lock_root_y {
            tf.translation.y = r.y;
        }
    }
}

/// After a few frames, log whether any `Vrm` entity exists (async load can delay spawn).
///
/// Embedded UIKit: `active_viewport_data` often never picks our camera, so we pin PanOrbit to the
/// primary 3D camera with [`ActiveCameraData::manual`]. **Critical:** we must clear
/// [`ActiveCameraData::entity`] whenever egui wants the pointer — [`absorb_bevy_input_system`] does
/// not clear Bevy [`Touches`], so one-finger orbit would otherwise steal drags from egui windows.
///
/// Runs in [`PostUpdate`] **after** [`EguiPostUpdateSet::ProcessOutput`] so [`EguiWantsInput`] reflects
/// the frame's egui pass, and **before** [`PanOrbitCameraSystemSet`] so PanOrbit sees a cleared camera.
fn sync_ios_panorbit_active_camera(
    mut active: ResMut<ActiveCameraData>,
    mut warned: Local<bool>,
    windows: Query<&Window, With<PrimaryWindow>>,
    cam_q: Query<Entity, (With<Camera3d>, With<PanOrbitCamera>)>,
    egui_wants: Res<EguiWantsInput>,
) {
    let Ok(window) = windows.single() else {
        return;
    };
    let cams: Vec<Entity> = cam_q.iter().collect();
    let entity = match cams.as_slice() {
        [e] => {
            *warned = false;
            *e
        }
        [] => {
            if !*warned {
                crate::jarvis_ios_line!(
                    "[JarvisIOS] panorbit sync: no Camera3d+PanOrbit entity (viewport may stay black)"
                );
                *warned = true;
            }
            return;
        }
        many => {
            if !*warned {
                crate::jarvis_ios_line!(
                    "[JarvisIOS] panorbit sync: {} Camera3d+PanOrbit entities (expected 1); using first",
                    many.len()
                );
                *warned = true;
            }
            many[0]
        }
    };
    let logical = Vec2::new(window.width(), window.height());
    if logical.x <= 1e-3 || logical.y <= 1e-3 {
        return;
    }
    let cam_entity = if egui_wants.wants_any_pointer_input() {
        None
    } else {
        Some(entity)
    };
    active.set_if_neq(ActiveCameraData {
        entity: cam_entity,
        viewport_size: Some(logical),
        window_size: Some(logical),
        manual: true,
    });
}

fn jarvis_ios_vrm_load_diag(
    mut frames: Local<u32>,
    mut highest_joint_count_logged: Local<usize>,
    mut prev_visible_count: Local<usize>,
    vrm_q: Query<(), With<Vrm>>,
    skinned_mesh_q: Query<(Entity, &SkinnedMesh), Added<SkinnedMesh>>,
    visibility_q: Query<(Entity, &SkinnedMesh, &bevy::camera::visibility::ViewVisibility)>,
    meshes: Res<Assets<Mesh>>,
    images: Res<Assets<Image>>,
    materials: Res<Assets<bevy::pbr::StandardMaterial>>,
) {
    *frames += 1;

    // Per-frame work (`SkinnedMesh added`, joint warnings) ALWAYS runs — these are one-shots
    // per entity, so the cost is bounded. The expensive work below (visibility iter, mem
    // snapshot, format_snapshot) is gated by verbosity: at NORMAL we still log every 60 frames
    // (~1 s) instead of every 30 frames, and we never log on visibility flicker (the previous
    // implementation logged on every flicker → up to 60 extra format!() + Mach traps per second
    // when bones moved across the camera frustum). At QUIET / OFF we skip everything.
    let mut highest_this_frame = 0usize;
    for (entity, sm) in &skinned_mesh_q {
        let joint_count = sm.joints.len();
        highest_this_frame = highest_this_frame.max(joint_count);
        crate::jarvis_ios_line!(
            "[JarvisIOS] SkinnedMesh added: entity={entity:?} joints={} {}",
            joint_count,
            if joint_count > 256 { "⚠ OVER_256_LIMIT" } else { "OK" },
        );
    }
    if highest_this_frame > *highest_joint_count_logged {
        *highest_joint_count_logged = highest_this_frame;
    }

    if !crate::debug_log::diag_logging_enabled() {
        return;
    }

    // Cadence: every frame in the boot window (≤30) so we have crash diagnostics, then every
    // 60 frames at NORMAL (was 30 before — halved log volume), every frame at DEBUG.
    let cadence = if crate::debug_log::debug_logging_enabled() { 1 } else { 60 };
    let should_log = *frames <= 30 || (*frames % cadence == 0);
    if !should_log {
        return;
    }

    // Compute visibility once, share between the count and max-joints calls.
    let mut visible_count = 0usize;
    let mut max_v = 0usize;
    for (_, sm, vv) in &visibility_q {
        if vv.get() {
            visible_count += 1;
            if sm.joints.len() > max_v {
                max_v = sm.joints.len();
            }
        }
    }
    *prev_visible_count = visible_count;

    let mem = crate::ios_mem_probe::format_snapshot();
    crate::jarvis_ios_line!(
        "[JarvisIOS] frame={}: visible_skinned={} max_visible_joints={} \
         assets[mesh={}, image={}, material={}] {}",
        *frames,
        visible_count,
        max_v,
        meshes.len(),
        images.len(),
        materials.len(),
        mem,
    );

    if *frames == 5 || *frames == 30 || *frames == 120 {
        let n = vrm_q.iter().count();
        crate::jarvis_ios_line!(
            "[JarvisIOS] diag: update_frame={} entities_with_Vrm={} highest_joint_count_seen={}",
            *frames,
            n,
            *highest_joint_count_logged,
        );
    }
}

/// Logs the end of every PostUpdate stage. Combined with the `app.update() leave` log,
/// this brackets the "extract → render-graph submit" window. If we see PostUpdate end
/// but no `leave`, the crash is in render-extract or GPU command submission.
fn jarvis_ios_post_update_marker(
    mut frames: Local<u32>,
    mut last_logged_frame: Local<u32>,
) {
    *frames += 1;
    if !crate::debug_log::diag_logging_enabled() {
        return;
    }
    // Boot window: log every frame for the first 30 to catch crash gaps. Steady-state: every
    // 120 frames (~2 s at 60Hz). DEBUG verbosity drops the gate to 30 (~0.5 s).
    let cadence = if crate::debug_log::debug_logging_enabled() { 30 } else { 120 };
    if *frames <= 30 || (*frames - *last_logged_frame) >= cadence {
        let mem = crate::ios_mem_probe::format_snapshot();
        crate::jarvis_ios_line!("[JarvisIOS] PostUpdate end: frame={} {}", *frames, mem);
        *last_logged_frame = *frames;
    }
}

fn clamp_vrm_root_y(settings: Res<IosAvatarSettings>, mut vrm_q: Query<&mut Transform, With<Vrm>>) {
    if !settings.lock_vrm_root_y {
        return;
    }
    let target_y = settings.world_position.y;
    for mut tf in &mut vrm_q {
        if (tf.translation.y - target_y).abs() > f32::EPSILON {
            tf.translation.y = target_y;
        }
    }
}

pub struct IosEmbeddedRenderer {
    app: App,
    /// Log render enter/leave for the first N frames only (diagnostics; avoid log spam).
    render_diag_frames_to_log: u8,
    /// After a panic in `app.update()`, Bevy state is undefined; skip further ticks (FFI must not unwind).
    render_poisoned: AtomicBool,
    /// Primary window entity for injected [`TouchInput`] messages.
    primary_window: Entity,
    /// UIKit touches forwarded from Swift; drained at the start of each `render()` / `app.update()`.
    touch_queue: Mutex<Vec<(u8, f32, f32, u64)>>,
    /// Re-read manifest + swap VRM (set from Swift after hub sync).
    profile_reload_pending: Mutex<bool>,
    /// VRMA paths relative to `JARVIS_ASSET_ROOT` (asset server root).
    vrma_play_queue: Mutex<Vec<(String, bool)>>,
    /// Pose-library JSON paths relative to `JARVIS_ASSET_ROOT` (last queued wins).
    json_anim_queue: Mutex<Vec<(String, bool)>>,
    /// Rolling buffer of recent `app.update()` durations in microseconds; used by the periodic
    /// histogram emit to surface stalls that the slow-frame detector misses (e.g. groups of
    /// frames hovering around 25 ms — visible as judder but never crossing the 32 ms gate).
    frame_us_ring: Mutex<Vec<u32>>,
    /// Frame index at which the histogram was last emitted (used together with
    /// `frame_us_ring` to log a summary every ~120 frames).
    last_hist_frame: u64,
    /// Monotonic frame counter for the renderer (independent of the Bevy `FrameCount` resource;
    /// we need it before `World` is set up too).
    render_frame_idx: u64,
}

impl IosEmbeddedRenderer {
    pub fn new(ui_view: *mut c_void, width_px: u32, height_px: u32, pixels_per_point: f32) -> Option<Self> {
        let view = NonNull::new(ui_view)?;
        let scale = pixels_per_point.max(0.5);

        let mut primary_window = Window::default();
        primary_window
            .resolution
            .set_physical_resolution(width_px.max(1), height_px.max(1));
        primary_window.resolution.set_scale_factor(scale);

        let asset_file_path = ios_asset_file_path();
        info!("JarvisIOS Bevy asset root (file_path): {asset_file_path}");
        crate::jarvis_ios_line!("[JarvisIOS] Bevy AssetPlugin file_path={asset_file_path}");
        if let Ok(p) = std::env::var("JARVIS_PROFILE_MANIFEST") {
            info!("JarvisIOS profile manifest: {p}");
            crate::jarvis_ios_line!("[JarvisIOS] JARVIS_PROFILE_MANIFEST={p}");
        } else {
            crate::jarvis_ios_line!("[JarvisIOS] JARVIS_PROFILE_MANIFEST unset");
        }

        let mut app = App::new();
        app.insert_resource(PendingIosSurface {
            view,
            width_px: width_px.max(1),
            height_px: height_px.max(1),
            scale_factor: scale,
        });
        let (avatar_settings, graphics_settings, spring_toml, mtoon_overrides_json, material_visibility_json) =
            crate::ios_profile_manifest::load_ios_hub_profile_bundle_from_env();
        crate::jarvis_ios_line!(
            "[JarvisIOS] IosAvatarSettings model_path={} idle_vrma_path={}",
            avatar_settings.model_path,
            avatar_settings.idle_vrma_path
        );
        let model_path = avatar_settings.model_path.clone();
        app.insert_resource(avatar_settings);
        app.insert_resource(graphics_settings);
        app.insert_resource(IosSpringPresetToml(spring_toml));
        app.insert_resource(IosMToonOverridesJson(mtoon_overrides_json));
        app.insert_resource(IosMaterialVisibilityJson(material_visibility_json.clone()));
        app.insert_resource(crate::ios_user_prefs::material_visibility_store_for_model(
            material_visibility_json.as_deref(),
            &model_path,
        ));
        app.init_resource::<IosAvatarRootEntity>();
        app.init_resource::<IosExpressionsState>();
        app.init_resource::<IosAnimationCatalog>();
        app.init_resource::<IosEguiAnimRequests>();

        app.add_plugins(
            DefaultPlugins
                .build()
                .set(AssetPlugin {
                    file_path: asset_file_path,
                    meta_check: AssetMetaCheck::Never,
                    ..default()
                })
                .disable::<WinitPlugin>()
                // Our file tracing subscriber (installed by jarvis_ios_set_log_file before
                // Bevy starts) handles all log output; disable Bevy's LogPlugin to avoid a
                // "global default already set" panic from the tracing crate.
                .disable::<bevy::log::LogPlugin>()
                .set(WindowPlugin {
                    primary_window: Some(primary_window),
                    primary_cursor_options: Some(CursorOptions::default()),
                    exit_condition: ExitCondition::DontExit,
                    close_when_requested: false,
                })
                .add_before::<RenderPlugin>(IosEmbedRawHandlesPlugin),
        );
        app.add_plugins((VrmPlugin, VrmaPlugin, EguiPlugin::default(), PanOrbitCameraPlugin));
        crate::ios_anim_json::plugin(&mut app);
        app.add_plugins(crate::ios_anim_layers::IosAnimLayersPlugin);
        app.add_plugins(crate::ios_device_motion::IosDeviceMotionPlugin);
        app.init_resource::<IosIdlePlaybackState>();
        // When the pointer is over egui (menu bar, windows), PanOrbit must not consume drags/pinch.
        let mut egui_global = bevy_egui::EguiGlobalSettings::default();
        egui_global.enable_absorb_bevy_input_system = true;
        app.insert_resource(egui_global);
        app.insert_resource(EguiFocusIncludesHover(true));
        app.init_resource::<crate::ios_egui_ui::JarvisIosUiState>();

        app.add_systems(
            Startup,
            (spawn_ios_viewport, spawn_ios_avatar).chain(),
        );
        app.add_systems(
            PostUpdate,
            (lock_hips_root_motion, clamp_vrm_root_y)
                .chain()
                .after(AnimationSystems),
        );
        app.add_systems(
            PostUpdate,
            sync_ios_panorbit_active_camera
                .after(EguiPostUpdateSet::ProcessOutput)
                .before(bevy_panorbit_camera::PanOrbitCameraSystemSet),
        );
        // PostUpdate end-marker runs LAST so we can see the schedule completed before render-extract.
        // Crash gap = (PostUpdate end logged) but NOT (app.update leave) → render extract / GPU submit.
        app.add_systems(Last, jarvis_ios_post_update_marker);
        // Drain egui-submitted animation requests right before the marker so user clicks in the
        // egui window queue clips for the same tick (no one-frame delay vs the FFI path).
        app.add_systems(Last, ios_drain_egui_anim_requests.before(jarvis_ios_post_update_marker));
        app.add_systems(
            Update,
            (
                log_render_device_limits,
                ios_apply_spring_preset_on_vrm_ready,
                ios_apply_mtoon_overrides_on_vrm_ready,
                ios_seed_material_visibility_on_vrm_ready,
                ios_apply_material_visibility,
                ios_collect_expression_presets,
                handle_deferred_vrm_load,
                jarvis_ios_vrm_load_diag,
                ios_refresh_animation_catalog,
            ),
        );
        app.add_systems(
            EguiPrimaryContextPass,
            (
                crate::ios_egui_ui::jarvis_ios_egui_apply_theme,
                crate::ios_egui_ui::jarvis_ios_egui_menu_bar,
                crate::ios_egui_ui::jarvis_ios_egui_windows,
            )
                .chain(),
        );

        // Without `WinitPlugin`, nothing runs Bevy's default `run_once` runner, which waits for
        // `PluginsState::Adding`, then calls `App::finish` / `cleanup` before the first `update()`.
        // `RenderPlugin::finish` inserts `RenderDevice` (and clones) on the **main** world; until
        // then, `bevy_pbr`'s `PostUpdate` systems like `no_automatic_skin_batching` see a missing
        // `Res<RenderDevice>` and fail strict validation (panic in debug builds).
        while app.plugins_state() == bevy::app::PluginsState::Adding {
            // iOS-only module: always tick pools (mirrors `bevy_app::run_once`); use `bevy::tasks`
            // so we do not need a direct `bevy_tasks` dependency.
            bevy::tasks::tick_global_task_pools_on_main_thread();
        }
        app.finish();
        app.cleanup();

        let primary_window = {
            let world = app.world_mut();
            let mut q = world.query_filtered::<Entity, With<PrimaryWindow>>();
            q.iter(world)
                .next()
                .expect("JarvisIOS: PrimaryWindow missing after Bevy init")
        };

        Some(Self {
            app,
            render_diag_frames_to_log: 30,
            render_poisoned: AtomicBool::new(false),
            primary_window,
            touch_queue: Mutex::new(Vec::new()),
            profile_reload_pending: Mutex::new(false),
            vrma_play_queue: Mutex::new(Vec::new()),
            json_anim_queue: Mutex::new(Vec::new()),
            frame_us_ring: Mutex::new(Vec::with_capacity(240)),
            last_hist_frame: 0,
            render_frame_idx: 0,
        })
    }

    pub fn note_render_poisoned(&self) {
        self.render_poisoned.store(true, Ordering::Release);
    }

    pub fn queue_touch(&self, phase: u8, x: f32, y: f32, id: u64) {
        if let Ok(mut g) = self.touch_queue.lock() {
            g.push((phase, x, y, id));
        }
    }

    pub fn queue_profile_reload(&self) {
        if let Ok(mut g) = self.profile_reload_pending.lock() {
            *g = true;
        }
    }

    pub fn queue_vrma_play(&self, path: String, loop_forever: bool) {
        if let Ok(mut g) = self.vrma_play_queue.lock() {
            g.push((path, loop_forever));
        }
    }

    pub fn queue_json_anim_play(&self, path: String, loop_forever: bool) {
        if let Ok(mut g) = self.json_anim_queue.lock() {
            g.push((path, loop_forever));
        }
    }

    pub fn set_device_motion(
        &mut self,
        gx: f32,
        gy: f32,
        gz: f32,
        ax: f32,
        ay: f32,
        az: f32,
        enabled: bool,
    ) {
        let should_reset = {
            let world = self.app.world_mut();
            let prev = world
                .resource::<crate::ios_device_motion::IosDeviceMotionInput>()
                .enabled;
            let mut m = world.resource_mut::<crate::ios_device_motion::IosDeviceMotionInput>();
            m.enabled = enabled;
            if enabled {
                m.gravity_dir = Vec3::new(gx, gy, gz);
                m.user_accel = Vec3::new(ax, ay, az);
                false
            } else {
                m.gravity_dir = Vec3::new(0.0, -1.0, 0.0);
                m.user_accel = Vec3::ZERO;
                prev
            }
        };
        if should_reset {
            crate::ios_device_motion::ios_reset_springs_after_device_motion_off(self.app.world_mut());
        }
    }

    pub fn set_device_motion_tuning(
        &mut self,
        gravity_blend: f32,
        max_tilt_deg: f32,
        shake_power: f32,
        max_shake_mult: f32,
        shake_deadzone: f32,
        spring_scope: u8,
        spring_gravity_scale: f32,
        spring_drag_scale: f32,
    ) {
        let mut t = self
            .app
            .world_mut()
            .resource_mut::<crate::ios_device_motion::IosDeviceMotionTuning>();
        t.phone_gravity_blend = gravity_blend.clamp(0.0, 1.0);
        t.max_tilt_from_down_rad = max_tilt_deg.clamp(5.0, 89.0).to_radians();
        t.shake_power_per_ms2 = shake_power.max(0.0);
        t.max_power_mult = max_shake_mult.max(1.0);
        t.shake_deadzone_ms2 = shake_deadzone.max(0.0);
        t.spring_scope = crate::ios_device_motion::IosSpringBoneScope::from_u8(spring_scope);
        t.spring_gravity_power_scale = spring_gravity_scale.clamp(0.0, 3.0);
        t.spring_drag_scale = spring_drag_scale.clamp(0.05, 5.0);
    }

    pub fn set_idle_animation_enabled(&mut self, enabled: bool) {
        ios_set_idle_animation_enabled(self.app.world_mut(), enabled);
    }

    pub fn expressions_snapshot_json(&self) -> String {
        let world = self.app.world();
        let state = world.resource::<IosExpressionsState>();
        let presets: Vec<serde_json::Value> = state
            .presets
            .iter()
            .map(|n| {
                serde_json::json!({
                    "name": n,
                    "weight": state.weights.get(n).copied().unwrap_or(0.0_f32),
                })
            })
            .collect();
        serde_json::json!({ "presets": presets }).to_string()
    }

    pub fn set_expression_weight(&mut self, name: &str, weight: f32) {
        let world = self.app.world_mut();
        let mut state = world.resource_mut::<IosExpressionsState>();
        let key = name.trim();
        if key.is_empty() {
            return;
        }
        state.weights.insert(key.to_string(), weight.clamp(0.0, 1.0));
    }

    pub fn apply_expressions_from_state(&mut self) {
        let world = self.app.world_mut();
        let weights_map = world.resource::<IosExpressionsState>().weights.clone();
        let mut vrm_q = world.query_filtered::<Entity, (With<Vrm>, With<Initialized>)>();
        let Some(vrm_e) = vrm_q.iter(world).next() else {
            return;
        };
        let weights: std::collections::HashMap<VrmExpression, f32> = weights_map
            .iter()
            .filter_map(|(k, &v)| {
                let n = k.trim();
                if n.is_empty() {
                    None
                } else {
                    Some((VrmExpression::from(n), v))
                }
            })
            .collect();
        world.commands().trigger(SetExpressions::from_iter(vrm_e, weights));
    }

    pub fn layers_snapshot_json(&self) -> String {
        let handle = self.app.world().resource::<crate::ios_anim_layers::IosLayerStackHandle>();
        crate::ios_anim_layers::layers_snapshot_json(handle)
    }

    pub fn layers_set_master(&mut self, enabled: bool) {
        let handle = self.app.world().resource::<crate::ios_anim_layers::IosLayerStackHandle>().clone();
        handle.with_write(|s| s.master_enabled = enabled);
    }

    pub fn layers_install_default(&mut self) {
        let handle = self.app.world().resource::<crate::ios_anim_layers::IosLayerStackHandle>().clone();
        handle.with_write(|s| {
            s.reset_and_install_default();
            s.master_enabled = true;
        });
    }

    pub fn layers_set_enabled(&mut self, id: u64, enabled: bool) {
        let handle = self.app.world().resource::<crate::ios_anim_layers::IosLayerStackHandle>().clone();
        handle.with_write(|s| {
            if let Some(l) = s.layers.iter_mut().find(|l| l.id == id) {
                l.enabled = enabled;
            }
        });
    }

    pub fn layers_set_weight(&mut self, id: u64, weight: f32) {
        let handle = self.app.world().resource::<crate::ios_anim_layers::IosLayerStackHandle>().clone();
        handle.with_write(|s| {
            if let Some(l) = s.layers.iter_mut().find(|l| l.id == id) {
                l.weight = weight.clamp(0.0, 1.0);
            }
        });
    }

    pub fn layers_clear(&mut self) {
        let handle = self.app.world().resource::<crate::ios_anim_layers::IosLayerStackHandle>().clone();
        handle.with_write(|s| s.layers.clear());
    }

    fn flush_queued_json_anim_requests(&mut self) {
        let drained: Vec<(String, bool)> = {
            let mut g = self.json_anim_queue.lock().unwrap();
            core::mem::take(&mut *g)
        };
        if drained.is_empty() {
            return;
        }
        let path = drained.into_iter().last().unwrap();
        ios_apply_json_anim_request(self.app.world_mut(), path.0, path.1);
    }

    fn flush_queued_vrma_requests(&mut self) {
        let drained: Vec<_> = {
            let mut g = self.vrma_play_queue.lock().unwrap();
            core::mem::take(&mut *g)
        };
        if drained.is_empty() {
            return;
        }
        ios_apply_vrma_requests(self.app.world_mut(), drained);
    }

    fn apply_hub_profile_reload(&mut self) {
        let (avatar, mut graphics, spring, mtoon, material_vis) =
            crate::ios_profile_manifest::load_ios_hub_profile_bundle_from_env();
        // Apply the same MSAA reduction as spawn_ios_viewport for large VRMs.
        let asset_root = ios_asset_file_path();
        let vrm_disk = Path::new(&asset_root).join(&avatar.model_path);
        if let Ok(meta) = std::fs::metadata(&vrm_disk) {
            if meta.len() >= VRM_WARN_BYTES && graphics.msaa_samples > 1 {
                graphics.msaa_samples = 1;
                crate::jarvis_ios_line!(
                    "[JarvisIOS] profile reload: large VRM ({:.1} MB) → MSAA forced off",
                    meta.len() as f64 / (1024.0 * 1024.0)
                );
            }
        }
        let world = self.app.world_mut();
        *world.resource_mut::<IosAvatarSettings>() = avatar.clone();
        *world.resource_mut::<IosGraphicsSettings>() = graphics.clone();
        *world.resource_mut::<IosSpringPresetToml>() = IosSpringPresetToml(spring);
        *world.resource_mut::<IosMToonOverridesJson>() = IosMToonOverridesJson(mtoon);
        *world.resource_mut::<IosMaterialVisibilityJson>() =
            IosMaterialVisibilityJson(material_vis.clone());
        *world.resource_mut::<IosMaterialVisibilityStore>() =
            crate::ios_user_prefs::material_visibility_store_for_model(
                material_vis.as_deref(),
                &avatar.model_path,
            );
        world.insert_resource(ClearColor(avatar.background_color));
        // Despawn every avatar root (VRM + VRMA children). Relying on a single stored entity can miss
        // duplicates if a previous reload partially failed, which breaks PanOrbit + leaves a black view.
        let roots: Vec<Entity> = world
            .query_filtered::<Entity, With<JarvisIosAvatarRoot>>()
            .iter(world)
            .collect();
        for e in roots {
            world.entity_mut(e).despawn();
        }
        world.flush();
        world.insert_resource(IosAvatarRootEntity(None));
        apply_scene_graphics_from_settings(world, &graphics, avatar.world_position);
        update_panorbit_camera_focus(world, avatar.world_position);
        let asset_server = world.resource::<AssetServer>().clone();
        let id = spawn_jarvis_ios_vrm_root(&mut world.commands(), &asset_server, &avatar);
        world.insert_resource(IosAvatarRootEntity(Some(id)));
        world.resource_mut::<crate::ios_anim_layers::IosBoneNameMap>().lower_to_entity.clear();
        world.resource_mut::<crate::ios_anim_layers::IosBoneNameMap>().vrm_entity = None;
        world.resource_mut::<crate::ios_anim_layers::IosRestPoseSnapshot>().captured = 0;
        world
            .resource_mut::<crate::ios_device_motion::IosSpringMotionBaselines>()
            .clear();
        world.resource_mut::<IosIdlePlaybackState>().paused_entities.clear();
        world.flush();
        let idle_enabled = world.resource::<IosIdlePlaybackState>().user_enabled;
        if !idle_enabled {
            ios_set_idle_animation_enabled(world, false);
        }
        world
            .resource_mut::<crate::ios_anim_json::IosJsonAnimPlayback>()
            .replace_with_clip(None);
        crate::jarvis_ios_line!(
            "[JarvisIOS] profile reload applied model_path={} msaa_samples={}",
            avatar.model_path,
            graphics.msaa_samples
        );
    }

    fn flush_queued_touch_inputs(&mut self) {
        let drained: Vec<_> = {
            let mut g = self.touch_queue.lock().unwrap();
            core::mem::take(&mut *g)
        };
        if drained.is_empty() {
            return;
        }
        let win = self.primary_window;
        let world = self.app.world_mut();
        for (phase, x, y, id) in drained {
            let phase = match phase {
                0 => TouchPhase::Started,
                1 => TouchPhase::Moved,
                2 => TouchPhase::Ended,
                _ => TouchPhase::Canceled,
            };
            world.write_message(TouchInput {
                phase,
                position: Vec2::new(x, y),
                window: win,
                force: None,
                id,
            });
        }
    }

    pub fn render(&mut self) {
        if self.render_poisoned.load(Ordering::Acquire) {
            return;
        }
        let t0 = std::time::Instant::now();
        if self.render_diag_frames_to_log > 0 {
            let mem = crate::ios_mem_probe::format_snapshot();
            crate::jarvis_ios_line!("[JarvisIOS] render: app.update() enter {mem}");
        }
        let reload = match self.profile_reload_pending.lock() {
            Ok(mut g) => core::mem::take(&mut *g),
            Err(_) => false,
        };
        if reload {
            self.apply_hub_profile_reload();
        }
        self.flush_queued_json_anim_requests();
        self.flush_queued_vrma_requests();
        self.flush_queued_touch_inputs();
        self.app.update();
        let elapsed = t0.elapsed();
        self.render_frame_idx += 1;

        // Maintain a rolling 240-sample frame-time ring (4 seconds at 60Hz).
        // Periodic histogram emit catches micro-stalls that the 32 ms gate misses.
        let elapsed_us = u32::try_from(elapsed.as_micros()).unwrap_or(u32::MAX);
        if let Ok(mut ring) = self.frame_us_ring.lock() {
            if ring.len() == 240 {
                ring.remove(0);
            }
            ring.push(elapsed_us);

            // Emit histogram every 120 frames (~2 s). Cheap arithmetic only — no heap allocs.
            if self.render_frame_idx - self.last_hist_frame >= 120 && ring.len() >= 60 {
                self.last_hist_frame = self.render_frame_idx;
                if crate::debug_log::diag_logging_enabled() {
                    let mut sorted: Vec<u32> = ring.clone();
                    sorted.sort_unstable();
                    let p50 = sorted[sorted.len() / 2];
                    let p95 = sorted[(sorted.len() * 95) / 100];
                    let p99 = sorted[(sorted.len() * 99) / 100];
                    let max = *sorted.last().unwrap_or(&0);
                    let avg: u64 = sorted.iter().map(|&v| v as u64).sum::<u64>() / sorted.len() as u64;
                    let over32 = sorted.iter().filter(|&&v| v >= 32_000).count();
                    let over16 = sorted.iter().filter(|&&v| v >= 16_700).count();
                    crate::jarvis_ios_line!(
                        "[JarvisIOS] frame_hist N={} avg={:.1}ms p50={:.1} p95={:.1} p99={:.1} max={:.1} hitches>=16ms={} >=32ms={}",
                        sorted.len(),
                        (avg as f32) / 1000.0,
                        (p50 as f32) / 1000.0,
                        (p95 as f32) / 1000.0,
                        (p99 as f32) / 1000.0,
                        (max as f32) / 1000.0,
                        over16,
                        over32,
                    );
                }
            }
        }

        if self.render_diag_frames_to_log > 0 {
            let ms = elapsed.as_millis();
            let mem = crate::ios_mem_probe::format_snapshot();
            crate::jarvis_ios_line!("[JarvisIOS] render: app.update() leave ({ms}ms) {mem}");
            self.render_diag_frames_to_log -= 1;
        } else if elapsed.as_millis() >= 32 && crate::debug_log::diag_logging_enabled() {
            // Steady-state slow-frame detector — anything ≥2 display refreshes is a visible stall.
            // Async logger keeps the channel send cheap, but the Mach trap behind format_snapshot()
            // costs ~100 µs each — gate it behind `diag_logging_enabled` so QUIET/OFF skips it.
            let mem = crate::ios_mem_probe::format_snapshot();
            crate::jarvis_ios_line!(
                "[JarvisIOS] render: SLOW frame {}ms (>=32ms) {}",
                elapsed.as_millis(),
                mem,
            );
        }
    }

    pub fn resize(&mut self, width_px: u32, height_px: u32) {
        let world = self.app.world_mut();
        let mut q = world.query_filtered::<&mut Window, With<PrimaryWindow>>();
        let Ok(mut window) = q.single_mut(world) else {
            return;
        };
        window
            .resolution
            .set_physical_resolution(width_px.max(1), height_px.max(1));
    }
}
