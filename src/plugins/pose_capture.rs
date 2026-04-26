//! Offscreen multi-view avatar capture for MCP pose verification.
//!
//! Captures transparent PNGs from deterministic camera views (front/left/right
//! and optional extras). The capture camera only renders entities on a dedicated
//! render layer so outputs contain only the avatar + lights, not the ground/UI.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use bevy::camera::visibility::RenderLayers;
use bevy::camera::{ClearColorConfig, RenderTarget};
use bevy::prelude::*;
use bevy::render::render_resource::TextureFormat;
use bevy::render::view::screenshot::{Screenshot, ScreenshotCaptured};
use bevy_vrm1::prelude::Vrm;
use crossbeam_channel::{unbounded, Receiver, Sender};
use serde::Serialize;

pub const AVATAR_CAPTURE_LAYER: usize = 7;

pub struct PoseCapturePlugin;

impl Plugin for PoseCapturePlugin {
    fn build(&self, app: &mut App) {
        let (req_tx, req_rx) = unbounded::<CaptureRequest>();
        let (view_tx, view_rx) = unbounded::<CaptureViewResult>();
        app.insert_resource(CaptureCommandSender(req_tx))
            .insert_resource(CaptureCommandQueue(req_rx))
            .insert_resource(CaptureViewResultQueue {
                tx: view_tx,
                rx: view_rx,
            })
            .init_resource::<CaptureSessions>()
            .add_systems(
                Update,
                (
                    sync_avatar_capture_layers,
                    drain_capture_requests,
                    process_capture_view_results,
                )
                    .chain(),
            );
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureView {
    Front,
    Left,
    Right,
    FrontLeft,
    FrontRight,
}

impl CaptureView {
    pub fn as_slug(&self) -> &'static str {
        match self {
            Self::Front => "front",
            Self::Left => "left",
            Self::Right => "right",
            Self::FrontLeft => "front_left",
            Self::FrontRight => "front_right",
        }
    }

    fn camera_direction(&self) -> Vec3 {
        match self {
            Self::Front => Vec3::new(0.0, 0.0, 1.0),
            Self::Left => Vec3::new(-1.0, 0.0, 0.0),
            Self::Right => Vec3::new(1.0, 0.0, 0.0),
            Self::FrontLeft => Vec3::new(-1.0, 0.0, 1.0).normalize(),
            Self::FrontRight => Vec3::new(1.0, 0.0, 1.0).normalize(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureFramingPreset {
    FullBody,
    FaceCloseup,
}

#[derive(Debug, Clone)]
pub struct CaptureCameraOverrides {
    pub focus_y_offset: Option<f32>,
    pub radius: Option<f32>,
    pub height_lift: Option<f32>,
}

#[derive(Debug, Clone)]
pub struct CaptureRequest {
    pub output_dir: PathBuf,
    pub capture_id: String,
    pub width: u32,
    pub height: u32,
    pub views: Vec<CaptureView>,
    pub framing_preset: Option<CaptureFramingPreset>,
    pub camera_overrides: Option<CaptureCameraOverrides>,
    pub response_tx: Sender<CaptureResult>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CaptureImage {
    pub view: CaptureView,
    pub path: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CaptureResult {
    pub capture_id: String,
    pub output_dir: String,
    pub width: u32,
    pub height: u32,
    pub framing_preset: Option<CaptureFramingPreset>,
    pub images: Vec<CaptureImage>,
    pub errors: Vec<String>,
}

#[derive(Resource, Clone)]
pub struct CaptureCommandSender(pub Sender<CaptureRequest>);

#[derive(Resource)]
struct CaptureCommandQueue(Receiver<CaptureRequest>);

#[derive(Resource)]
struct CaptureViewResultQueue {
    tx: Sender<CaptureViewResult>,
    rx: Receiver<CaptureViewResult>,
}

#[derive(Component)]
struct AvatarCaptureTagged;

#[derive(Component)]
struct CaptureCameraTag;

#[derive(Resource, Default)]
struct CaptureSessions {
    next_id: u64,
    sessions: HashMap<u64, CaptureSession>,
}

struct CaptureSession {
    request: CaptureRequest,
    pending_views: usize,
    images: Vec<CaptureImage>,
    errors: Vec<String>,
    camera_entities: Vec<Entity>,
}

struct CaptureViewResult {
    session_id: u64,
    view: CaptureView,
    path: PathBuf,
    error: Option<String>,
}

#[derive(Debug, Clone, Copy)]
struct CaptureCameraProfile {
    focus_y_offset: f32,
    radius: f32,
    height_lift: f32,
}

impl CaptureCameraProfile {
    fn default_profile() -> Self {
        Self {
            focus_y_offset: 1.35,
            radius: 1.75,
            height_lift: 0.15,
        }
    }

    fn from_preset(preset: CaptureFramingPreset) -> Self {
        match preset {
            // Pull camera back and lower focus so heels/lower legs stay in frame.
            CaptureFramingPreset::FullBody => Self {
                focus_y_offset: 0.95,
                radius: 2.35,
                height_lift: 0.12,
            },
            // Tight framing around face/head for expression QA.
            CaptureFramingPreset::FaceCloseup => Self {
                focus_y_offset: 1.58,
                radius: 0.72,
                height_lift: 0.03,
            },
        }
    }

    fn with_overrides(self, overrides: &CaptureCameraOverrides) -> Self {
        Self {
            focus_y_offset: overrides
                .focus_y_offset
                .unwrap_or(self.focus_y_offset)
                .clamp(0.2, 3.0),
            radius: overrides.radius.unwrap_or(self.radius).clamp(0.3, 8.0),
            height_lift: overrides
                .height_lift
                .unwrap_or(self.height_lift)
                .clamp(-1.0, 2.0),
        }
    }
}

fn sync_avatar_capture_layers(
    mut commands: Commands,
    vrm_q: Query<Entity, With<Vrm>>,
    children_q: Query<&Children>,
    mut layer_queries: ParamSet<(
        Query<Option<&mut RenderLayers>>,
        Query<(Entity, Option<&mut RenderLayers>), With<DirectionalLight>>,
    )>,
    tag_q: Query<(), With<AvatarCaptureTagged>>,
) {
    for vrm_root in &vrm_q {
        let mut stack = vec![vrm_root];
        while let Some(entity) = stack.pop() {
            if tag_q.get(entity).is_err() {
                if let Ok(maybe_layers) = layer_queries.p0().get_mut(entity) {
                    let layers = maybe_layers
                        .map(|l| l.clone().with(AVATAR_CAPTURE_LAYER))
                        .unwrap_or_else(|| RenderLayers::from_layers(&[0, AVATAR_CAPTURE_LAYER]));
                    commands
                        .entity(entity)
                        .insert((layers, AvatarCaptureTagged));
                }
            }
            if let Ok(children) = children_q.get(entity) {
                for child in children.iter() {
                    stack.push(child);
                }
            }
        }
    }

    // Ensure the main directional lights affect both the regular and capture
    // layers so the offscreen avatar renders with the same shading.
    for (entity, maybe_layers) in &mut layer_queries.p1() {
        let layers = maybe_layers
            .map(|l| l.clone().with(AVATAR_CAPTURE_LAYER))
            .unwrap_or_else(|| RenderLayers::from_layers(&[0, AVATAR_CAPTURE_LAYER]));
        commands.entity(entity).insert(layers);
    }
}

fn drain_capture_requests(
    mut commands: Commands,
    queue: Res<CaptureCommandQueue>,
    result_queue: Res<CaptureViewResultQueue>,
    mut sessions: ResMut<CaptureSessions>,
    vrm_q: Query<&GlobalTransform, With<Vrm>>,
    mut images: ResMut<Assets<Image>>,
) {
    while let Ok(req) = queue.0.try_recv() {
        if req.views.is_empty() {
            let _ = req.response_tx.send(CaptureResult {
                capture_id: req.capture_id,
                output_dir: req.output_dir.display().to_string(),
                width: req.width,
                height: req.height,
                framing_preset: req.framing_preset,
                images: Vec::new(),
                errors: vec!["at least one view is required".to_string()],
            });
            continue;
        }
        if let Err(err) = std::fs::create_dir_all(&req.output_dir) {
            let _ = req.response_tx.send(CaptureResult {
                capture_id: req.capture_id,
                output_dir: req.output_dir.display().to_string(),
                width: req.width,
                height: req.height,
                framing_preset: req.framing_preset,
                images: Vec::new(),
                errors: vec![format!("create_dir_all failed: {err}")],
            });
            continue;
        }
        let Ok(vrm_tf) = vrm_q.single() else {
            let _ = req.response_tx.send(CaptureResult {
                capture_id: req.capture_id,
                output_dir: req.output_dir.display().to_string(),
                width: req.width,
                height: req.height,
                framing_preset: req.framing_preset,
                images: Vec::new(),
                errors: vec!["avatar root not ready".to_string()],
            });
            continue;
        };
        let mut camera_profile = req
            .framing_preset
            .map(CaptureCameraProfile::from_preset)
            .unwrap_or_else(CaptureCameraProfile::default_profile);
        if let Some(overrides) = &req.camera_overrides {
            camera_profile = camera_profile.with_overrides(overrides);
        }

        let focus = vrm_tf.translation() + Vec3::Y * camera_profile.focus_y_offset;
        let radius = camera_profile.radius;
        let height_lift = camera_profile.height_lift;
        let session_id = sessions.next_id;
        sessions.next_id = sessions.next_id.saturating_add(1);
        let mut camera_entities = Vec::with_capacity(req.views.len());

        for view in &req.views {
            let render_target =
                Image::new_target_texture(req.width, req.height, TextureFormat::Rgba8UnormSrgb, None);
            let image_handle = images.add(render_target);
            let eye = focus + view.camera_direction() * radius + Vec3::Y * height_lift;
            let camera = commands
                .spawn((
                    Camera3d::default(),
                    Camera {
                        clear_color: ClearColorConfig::Custom(Color::linear_rgba(0.0, 0.0, 0.0, 0.0)),
                        ..default()
                    },
                    RenderTarget::Image(image_handle.clone().into()),
                    Transform::from_translation(eye).looking_at(focus, Vec3::Y),
                    RenderLayers::layer(AVATAR_CAPTURE_LAYER),
                    CaptureCameraTag,
                ))
                .id();
            camera_entities.push(camera);

            let png_path = req.output_dir.join(format!(
                "{}_{}_{}x{}.png",
                req.capture_id,
                view.as_slug(),
                req.width,
                req.height
            ));
            let tx = result_queue.tx.clone();
            let view_for_cb = view.clone();
            commands
                .spawn(Screenshot::image(image_handle))
                .observe(move |ev: On<ScreenshotCaptured>| {
                    let mut err = None;
                    if let Err(e) = save_rgba_png(Path::new(&png_path), &ev.image) {
                        err = Some(e);
                    }
                    let _ = tx.send(CaptureViewResult {
                        session_id,
                        view: view_for_cb.clone(),
                        path: png_path.clone(),
                        error: err,
                    });
                });
        }

        sessions.sessions.insert(
            session_id,
            CaptureSession {
                pending_views: req.views.len(),
                request: req,
                images: Vec::new(),
                errors: Vec::new(),
                camera_entities,
            },
        );
    }
}

fn process_capture_view_results(
    mut commands: Commands,
    queue: Res<CaptureViewResultQueue>,
    mut sessions: ResMut<CaptureSessions>,
) {
    while let Ok(result) = queue.rx.try_recv() {
        let Some(session) = sessions.sessions.get_mut(&result.session_id) else {
            continue;
        };
        if let Some(err) = result.error {
            session
                .errors
                .push(format!("{}: {err}", result.view.as_slug()));
        } else {
            session.images.push(CaptureImage {
                view: result.view,
                path: result.path.display().to_string(),
            });
        }
        session.pending_views = session.pending_views.saturating_sub(1);

        if session.pending_views == 0 {
            for camera in &session.camera_entities {
                commands.entity(*camera).despawn();
            }
            let output = CaptureResult {
                capture_id: session.request.capture_id.clone(),
                output_dir: session.request.output_dir.display().to_string(),
                width: session.request.width,
                height: session.request.height,
                framing_preset: session.request.framing_preset,
                images: session.images.clone(),
                errors: session.errors.clone(),
            };
            let _ = session.request.response_tx.send(output);
        }
    }
    sessions.sessions.retain(|_, s| s.pending_views > 0);
}

fn save_rgba_png(path: &Path, image: &Image) -> Result<(), String> {
    let dynamic = image
        .clone()
        .try_into_dynamic()
        .map_err(|e| format!("bevy image conversion failed: {e}"))?;
    dynamic
        .to_rgba8()
        .save(path)
        .map_err(|e| format!("save png failed: {e}"))?;
    Ok(())
}
