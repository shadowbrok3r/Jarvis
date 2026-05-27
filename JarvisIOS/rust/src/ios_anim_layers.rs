//! Procedural animation layers for iOS (breathing, blink, fidgets).
//! Mirrors desktop `anim_layers` composition; applies bones via `Transform` + `SetExpressions`.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use bevy::app::AnimationSystems;
use bevy::prelude::*;
use bevy_vrm1::prelude::{
    Initialized, RestGlobalTransform, RestTransform, SetExpressions, Vrm, VrmExpression, VrmSystemSets,
};
use rand::Rng;

use crate::ios_bevy::IosAvatarRootEntity;

pub struct IosAnimLayersPlugin;

impl Plugin for IosAnimLayersPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(IosLayerStackHandle::default())
            .insert_resource(IosRestPoseSnapshot::default())
            .insert_resource(IosBoneNameMap::default())
            .add_systems(Startup, ios_auto_install_default_layers)
            .add_systems(
                PostUpdate,
                (
                    ios_refresh_bone_map.after(AnimationSystems),
                    ios_refresh_rest_pose.after(ios_refresh_bone_map),
                    ios_advance_anim_layers
                        .after(ios_refresh_rest_pose)
                        .before(VrmSystemSets::Constraints),
                ),
            );
    }
}

#[derive(Resource, Clone, Default)]
pub struct IosLayerStackHandle {
    pub inner: Arc<RwLock<IosLayerStack>>,
}

impl IosLayerStackHandle {
    pub fn with_write<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut IosLayerStack) -> R,
    {
        let mut guard = self.inner.write().expect("ios layer stack lock");
        f(&mut guard)
    }

    pub fn with_read<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&IosLayerStack) -> R,
    {
        let guard = self.inner.read().expect("ios layer stack lock");
        f(&*guard)
    }
}

#[derive(Debug, Default)]
pub struct IosLayerStack {
    pub master_enabled: bool,
    pub layers: Vec<IosLayer>,
    pub clock: f32,
    next_id: u64,
}

impl IosLayerStack {
    pub fn add_layer(&mut self, mut layer: IosLayer) -> u64 {
        self.next_id = self.next_id.saturating_add(1);
        layer.id = self.next_id;
        let id = layer.id;
        self.layers.push(layer);
        id
    }

    pub fn reset_and_install_default(&mut self) {
        self.layers.clear();
        self.next_id = 0;
        self.install_default_procedural_layers();
    }

    pub fn install_default_procedural_layers(&mut self) {
        for layer in [
            IosLayer::new("breathing", "Breathing", IosDriverKind::breathing_default())
                .blend(IosBlendMode::RestRelative)
                .weight(1.0),
            IosLayer::new("auto-blink", "Auto-Blink", IosDriverKind::blink_default())
                .blend(IosBlendMode::Override)
                .weight(1.0),
            IosLayer::new(
                "weight-shift",
                "Weight Shift",
                IosDriverKind::weight_shift_default(),
            )
            .blend(IosBlendMode::RestRelative)
            .weight(0.8),
            IosLayer::new(
                "finger-fidget",
                "Finger Fidget",
                IosDriverKind::finger_fidget_default(),
            )
            .blend(IosBlendMode::RestRelative)
            .weight(0.6),
            IosLayer::new("toe-fidget", "Toe Fidget", IosDriverKind::toe_fidget_default())
                .blend(IosBlendMode::RestRelative)
                .weight(0.4),
        ] {
            self.add_layer(layer);
        }
    }
}

#[derive(Debug, Clone)]
pub struct IosLayer {
    pub id: u64,
    pub slug: String,
    pub label: String,
    pub driver: IosDriverKind,
    pub weight: f32,
    pub enabled: bool,
    pub blend_mode: IosBlendMode,
    pub time: f32,
    pub speed: f32,
    pub playing: bool,
}

impl IosLayer {
    pub fn new(slug: impl Into<String>, label: impl Into<String>, driver: IosDriverKind) -> Self {
        Self {
            id: 0,
            slug: slug.into(),
            label: label.into(),
            driver,
            weight: 1.0,
            enabled: true,
            blend_mode: IosBlendMode::Override,
            time: 0.0,
            speed: 1.0,
            playing: true,
        }
    }

    pub fn weight(mut self, w: f32) -> Self {
        self.weight = w;
        self
    }

    pub fn blend(mut self, mode: IosBlendMode) -> Self {
        self.blend_mode = mode;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IosBlendMode {
    Override,
    RestRelative,
}

#[derive(Debug, Clone)]
pub enum IosDriverKind {
    Breathing {
        rate_hz: f32,
        pitch_deg: f32,
        roll_deg: f32,
    },
    Blink {
        next_in: f32,
        phase: IosBlinkPhase,
        phase_t: f32,
        mean_interval: f32,
        double_blink_chance: f32,
    },
    WeightShift {
        rate_hz: f32,
        hip_roll_deg: f32,
        spine_counter_deg: f32,
    },
    FingerFidget {
        amplitude_deg: f32,
        frequency_hz: f32,
        seed: u64,
    },
    ToeFidget {
        amplitude_deg: f32,
        frequency_hz: f32,
        seed: u64,
    },
}

impl IosDriverKind {
    pub fn breathing_default() -> Self {
        Self::Breathing {
            rate_hz: 0.22,
            pitch_deg: 1.2,
            roll_deg: 0.6,
        }
    }

    pub fn blink_default() -> Self {
        Self::Blink {
            next_in: 2.5,
            phase: IosBlinkPhase::Idle,
            phase_t: 0.0,
            mean_interval: 3.5,
            double_blink_chance: 0.12,
        }
    }

    pub fn weight_shift_default() -> Self {
        Self::WeightShift {
            rate_hz: 0.08,
            hip_roll_deg: 2.5,
            spine_counter_deg: 1.0,
        }
    }

    pub fn finger_fidget_default() -> Self {
        Self::FingerFidget {
            amplitude_deg: 4.0,
            frequency_hz: 0.35,
            seed: 0xC0FF_EE01,
        }
    }

    pub fn toe_fidget_default() -> Self {
        Self::ToeFidget {
            amplitude_deg: 3.0,
            frequency_hz: 0.25,
            seed: 0xFEED_FACE,
        }
    }

    fn kind_label(&self) -> &'static str {
        match self {
            Self::Breathing { .. } => "breathing",
            Self::Blink { .. } => "blink",
            Self::WeightShift { .. } => "weight_shift",
            Self::FingerFidget { .. } => "finger_fidget",
            Self::ToeFidget { .. } => "toe_fidget",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IosBlinkPhase {
    Idle,
    Close,
    Hold,
    Open,
}

#[derive(Resource, Default)]
pub(crate) struct IosRestPoseSnapshot {
    pub rest: HashMap<String, Quat>,
    pub rest_world: HashMap<String, Quat>,
    pub captured: usize,
    /// Bone-map generation we captured from. When the bone map is rebuilt
    /// (e.g. VRM hot-swap, VRMA load adds named child entities, layer
    /// clear forces a refresh), `captured_generation` is left behind and
    /// the next `ios_refresh_rest_pose` re-captures cleanly.
    pub captured_generation: u64,
}

#[derive(Resource, Default)]
pub(crate) struct IosBoneNameMap {
    pub lower_to_entity: HashMap<String, Entity>,
    pub vrm_entity: Option<Entity>,
    /// Monotonic counter bumped every time `lower_to_entity` is rebuilt.
    /// Drives rest-snapshot invalidation — see [`IosRestPoseSnapshot`].
    pub generation: u64,
}

impl IosBoneNameMap {
    /// Reset to "unmapped" state. The next `ios_refresh_bone_map` tick will
    /// rebuild from the current world.
    pub fn invalidate(&mut self) {
        self.lower_to_entity.clear();
        self.vrm_entity = None;
        self.generation = self.generation.wrapping_add(1);
    }
}

impl IosRestPoseSnapshot {
    pub fn invalidate(&mut self) {
        self.rest.clear();
        self.rest_world.clear();
        self.captured = 0;
        self.captured_generation = 0;
    }
}

fn ios_auto_install_default_layers(stack: Res<IosLayerStackHandle>) {
    stack.with_write(|s| {
        if !s.layers.is_empty() {
            return;
        }
        s.install_default_procedural_layers();
        s.master_enabled = true;
    });
}

fn ios_refresh_bone_map(
    avatar: Res<IosAvatarRootEntity>,
    vrm_q: Query<Entity, (With<Vrm>, With<Initialized>)>,
    mut map: ResMut<IosBoneNameMap>,
    children: Query<&Children>,
    names: Query<&Name>,
    rest_q: Query<&RestTransform>,
) {
    let Ok(vrm_e) = vrm_q.single() else { return };
    let Some(root) = avatar.0 else { return };
    if map.vrm_entity == Some(vrm_e) && !map.lower_to_entity.is_empty() {
        return;
    }
    map.lower_to_entity.clear();
    // CRITICAL: only collect entities that carry `RestTransform`. Those are
    // the canonical VRM bind-pose bones tagged by `bevy_vrm1`. Without this
    // filter, every named entity under the avatar root (including VRMA-clip
    // imported entities, accessory meshes with name labels, etc.) ended up
    // in the map. The layer system then tried to write rotations to those
    // entities using a bogus rest_world frame, producing the upside-down
    // flip + visible mis-rotations that the user reported.
    visit_named_bones(root, &children, &names, &rest_q, &mut map.lower_to_entity);
    map.vrm_entity = Some(vrm_e);
    map.generation = map.generation.wrapping_add(1);
}

fn visit_named_bones(
    e: Entity,
    children: &Query<&Children>,
    names: &Query<&Name>,
    rest_q: &Query<&RestTransform>,
    out: &mut HashMap<String, Entity>,
) {
    // Two conditions to be a bind-pose bone: has a `Name` AND has the
    // `RestTransform` component that `bevy_vrm1` stamps on every VRM bone
    // (`SkinnedMesh::joints` ancestors).
    if let Ok(n) = names.get(e) {
        if rest_q.get(e).is_ok() {
            out.insert(n.as_str().to_ascii_lowercase(), e);
        }
    }
    if let Ok(ch) = children.get(e) {
        for &child in ch {
            visit_named_bones(child, children, names, rest_q, out);
        }
    }
}

fn ios_refresh_rest_pose(
    map: Res<IosBoneNameMap>,
    mut snap: ResMut<IosRestPoseSnapshot>,
    rest_q: Query<&RestTransform>,
    rest_global_q: Query<&RestGlobalTransform>,
) {
    if map.lower_to_entity.is_empty() {
        return;
    }
    // Skip when we already captured from the current map generation — the
    // count comparison alone was unsafe because VRMA child entities can
    // come and go at the same total count, silently switching the rest
    // pose to a different bone set frame-to-frame.
    if snap.captured_generation == map.generation && !snap.rest.is_empty() {
        return;
    }
    let mut rest = HashMap::with_capacity(map.lower_to_entity.len());
    let mut rest_world = HashMap::with_capacity(map.lower_to_entity.len());
    for (name, &ent) in &map.lower_to_entity {
        if let Ok(rt) = rest_q.get(ent) {
            rest.insert(name.clone(), rt.0.rotation);
            let rw = rest_global_q
                .get(ent)
                .map(|rgt| rgt.0.rotation())
                .unwrap_or(Quat::IDENTITY);
            rest_world.insert(name.clone(), rw);
        }
    }
    snap.rest = rest;
    snap.rest_world = rest_world;
    snap.captured = map.lower_to_entity.len();
    snap.captured_generation = map.generation;
}

struct IosDriverSample {
    bones: HashMap<String, Quat>,
    expressions: HashMap<String, f32>,
}

fn ios_advance_anim_layers(
    time: Res<Time>,
    stack: Res<IosLayerStackHandle>,
    snap: Res<IosRestPoseSnapshot>,
    map: Res<IosBoneNameMap>,
    mut transforms: Query<(
        &mut Transform,
        Option<&RestTransform>,
        Option<&RestGlobalTransform>,
    )>,
    mut commands: Commands,
) {
    let Some(vrm_e) = map.vrm_entity else { return };
    if snap.rest.is_empty() || map.lower_to_entity.is_empty() {
        return;
    }
    let dt = time.delta_secs().min(0.05);

    let mut bones_out: HashMap<String, Quat> = HashMap::new();
    let mut expressions_out: HashMap<String, f32> = HashMap::new();

    stack.with_write(|layers| {
        layers.clock += dt;
        if !layers.master_enabled {
            return;
        }

        let mut accumulator = snap.rest.clone();

        for layer in &mut layers.layers {
            if !layer.enabled || layer.weight <= 0.0 {
                continue;
            }
            if layer.playing {
                layer.time += dt * layer.speed;
            }

            let sample = sample_ios_driver(&mut layer.driver, layer.time, dt);
            let weight = layer.weight.clamp(0.0, 1.0);

            for (bone, quat) in sample.bones {
                let bone_key = bone.to_ascii_lowercase();
                let rest = snap.rest.get(&bone_key).copied().unwrap_or(Quat::IDENTITY);
                let current = accumulator.get(&bone_key).copied().unwrap_or(rest);
                let folded = match layer.blend_mode {
                    IosBlendMode::Override => current.slerp(quat, weight),
                    IosBlendMode::RestRelative => {
                        let scaled = Quat::IDENTITY.slerp(quat, weight);
                        current * scaled
                    }
                };
                accumulator.insert(bone_key, folded);
            }

            for (name, w) in sample.expressions {
                let entry = expressions_out.entry(name).or_insert(0.0);
                *entry = (*entry + w * weight).clamp(0.0, 1.0);
            }
        }

        for (name, q_raw) in accumulator {
            let rest_local = snap.rest.get(&name).copied().unwrap_or(Quat::IDENTITY);
            if quat_close(q_raw, rest_local, 1e-4) {
                continue;
            }
            bones_out.insert(name, q_raw);
        }
    });

    for (bone_name, q_raw) in bones_out {
        let Some(&ent) = map.lower_to_entity.get(&bone_name) else {
            continue;
        };
        let Ok((mut tf, rest, rest_world)) = transforms.get_mut(ent) else {
            continue;
        };
        let final_q = match (rest, rest_world) {
            (Some(rt), Some(rgt)) => {
                let rest_local = rt.0.rotation;
                let rw = rgt.0.rotation();
                local_from_normalized(rest_local, rw, q_raw)
            }
            _ => q_raw,
        };
        if final_q.x.is_finite()
            && final_q.y.is_finite()
            && final_q.z.is_finite()
            && final_q.w.is_finite()
        {
            tf.rotation = final_q.normalize();
        }
    }

    if !expressions_out.is_empty() {
        let weights: HashMap<VrmExpression, f32> = expressions_out
            .into_iter()
            .map(|(k, v)| (VrmExpression::from(k.as_str()), v))
            .collect();
        commands.trigger(SetExpressions::from_iter(vrm_e, weights));
    }
}

#[inline]
fn local_from_normalized(rest_local: Quat, rest_world: Quat, pose_q: Quat) -> Quat {
    rest_local * rest_world.inverse() * pose_q * rest_world
}

fn quat_close(a: Quat, b: Quat, eps: f32) -> bool {
    (1.0 - a.dot(b).abs()).abs() <= eps
}

fn sample_ios_driver(driver: &mut IosDriverKind, t: f32, dt: f32) -> IosDriverSample {
    match driver {
        IosDriverKind::Breathing {
            rate_hz,
            pitch_deg,
            roll_deg,
        } => sample_breathing(t, *rate_hz, *pitch_deg, *roll_deg),
        IosDriverKind::Blink {
            next_in,
            phase,
            phase_t,
            mean_interval,
            double_blink_chance,
        } => sample_blink(dt, next_in, phase, phase_t, *mean_interval, *double_blink_chance),
        IosDriverKind::WeightShift {
            rate_hz,
            hip_roll_deg,
            spine_counter_deg,
        } => sample_weight_shift(t, *rate_hz, *hip_roll_deg, *spine_counter_deg),
        IosDriverKind::FingerFidget {
            amplitude_deg,
            frequency_hz,
            seed,
        } => sample_finger_fidget(t, *amplitude_deg, *frequency_hz, *seed),
        IosDriverKind::ToeFidget {
            amplitude_deg,
            frequency_hz,
            seed,
        } => sample_toe_fidget(t, *amplitude_deg, *frequency_hz, *seed),
    }
}

fn sample_breathing(t: f32, rate_hz: f32, pitch_deg: f32, roll_deg: f32) -> IosDriverSample {
    let omega = std::f32::consts::TAU * rate_hz;
    let pitch = (omega * t).sin() * pitch_deg.to_radians();
    let roll = (omega * t + std::f32::consts::FRAC_PI_2).sin() * roll_deg.to_radians();
    let mut bones = HashMap::new();
    bones.insert("chest".into(), Quat::from_euler(EulerRot::XYZ, pitch, 0.0, roll));
    bones.insert(
        "upperchest".into(),
        Quat::from_euler(EulerRot::XYZ, pitch * 0.4, 0.0, -roll * 0.3),
    );
    IosDriverSample {
        bones,
        expressions: HashMap::new(),
    }
}

fn sample_blink(
    dt: f32,
    next_in: &mut f32,
    phase: &mut IosBlinkPhase,
    phase_t: &mut f32,
    mean_interval: f32,
    double_blink_chance: f32,
) -> IosDriverSample {
    const CLOSE: f32 = 0.06;
    const OPEN: f32 = 0.12;
    let mut weight = 0.0;
    match *phase {
        IosBlinkPhase::Idle => {
            *next_in -= dt;
            if *next_in <= 0.0 {
                *phase = IosBlinkPhase::Close;
                *phase_t = 0.0;
            }
        }
        IosBlinkPhase::Close => {
            *phase_t += dt;
            weight = (*phase_t / CLOSE).clamp(0.0, 1.0);
            if *phase_t >= CLOSE {
                *phase = IosBlinkPhase::Hold;
                *phase_t = 0.0;
            }
        }
        IosBlinkPhase::Hold => {
            *phase_t += dt;
            weight = 1.0;
            if *phase_t >= 0.03 + mean_interval * 0.01 {
                *phase = IosBlinkPhase::Open;
                *phase_t = 0.0;
            }
        }
        IosBlinkPhase::Open => {
            *phase_t += dt;
            weight = 1.0 - (*phase_t / OPEN).clamp(0.0, 1.0);
            if *phase_t >= OPEN {
                *phase = IosBlinkPhase::Idle;
                *phase_t = 0.0;
                let mut rng = rand::rng();
                let base = mean_interval.max(0.5);
                let jitter: f32 = rng.random_range(0.5_f32..1.5);
                let mut next = base * jitter;
                if rng.random_bool(double_blink_chance as f64) {
                    next = 0.25 + rng.random_range(0.0_f32..0.3);
                }
                *next_in = next;
            }
        }
    }
    let eased = (weight * std::f32::consts::FRAC_PI_2).sin();
    let mut expressions = HashMap::new();
    expressions.insert("blink".into(), eased);
    IosDriverSample {
        bones: HashMap::new(),
        expressions,
    }
}

fn sample_weight_shift(
    t: f32,
    rate_hz: f32,
    hip_roll_deg: f32,
    spine_counter_deg: f32,
) -> IosDriverSample {
    let phase = (std::f32::consts::TAU * rate_hz * t).sin();
    let mut bones = HashMap::new();
    bones.insert(
        "hips".into(),
        Quat::from_euler(EulerRot::XYZ, 0.0, 0.0, phase * hip_roll_deg.to_radians()),
    );
    bones.insert(
        "spine".into(),
        Quat::from_euler(
            EulerRot::XYZ,
            0.0,
            0.0,
            -phase * spine_counter_deg.to_radians(),
        ),
    );
    IosDriverSample {
        bones,
        expressions: HashMap::new(),
    }
}

const FINGER_BONES: &[&str] = &[
    "leftthumbproximal",
    "leftindexintermediate",
    "leftmiddleintermediate",
    "leftringintermediate",
    "leftlittleintermediate",
    "rightthumbproximal",
    "rightindexintermediate",
    "rightmiddleintermediate",
    "rightringintermediate",
    "rightlittleintermediate",
];

fn sample_finger_fidget(t: f32, amplitude_deg: f32, frequency_hz: f32, seed: u64) -> IosDriverSample {
    let mut bones = HashMap::new();
    let amp = amplitude_deg.to_radians();
    for (i, name) in FINGER_BONES.iter().enumerate() {
        let phase_offset = hash_phase(seed, i as u64);
        let omega = std::f32::consts::TAU * (frequency_hz * (0.8 + (i as f32 * 0.07) % 0.5));
        let curl = (omega * t + phase_offset).sin() * amp;
        bones.insert((*name).to_string(), Quat::from_rotation_x(curl));
    }
    IosDriverSample {
        bones,
        expressions: HashMap::new(),
    }
}

const TOE_BONES: &[&str] = &["lefttoes", "righttoes"];

fn sample_toe_fidget(t: f32, amplitude_deg: f32, frequency_hz: f32, seed: u64) -> IosDriverSample {
    let mut bones = HashMap::new();
    let amp = amplitude_deg.to_radians();
    for (i, name) in TOE_BONES.iter().enumerate() {
        let phase_offset = hash_phase(seed, (i as u64) ^ 0xA5);
        let curl = (std::f32::consts::TAU * frequency_hz * t + phase_offset).sin() * amp;
        bones.insert((*name).to_string(), Quat::from_rotation_x(curl));
    }
    IosDriverSample {
        bones,
        expressions: HashMap::new(),
    }
}

fn hash_phase(seed: u64, idx: u64) -> f32 {
    let mut x = seed.wrapping_add(idx.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    x ^= x >> 30;
    x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    ((x as f32) / (u64::MAX as f32)) * std::f32::consts::TAU
}

/// JSON snapshot for Swift UI: `{ "masterEnabled": bool, "layers": [{ "id", "label", "kind", "enabled", "weight" }] }`
pub fn layers_snapshot_json(handle: &IosLayerStackHandle) -> String {
    handle.with_read(|stack| {
        let layers: Vec<serde_json::Value> = stack
            .layers
            .iter()
            .map(|l| {
                serde_json::json!({
                    "id": l.id,
                    "slug": l.slug,
                    "label": l.label,
                    "kind": l.driver.kind_label(),
                    "enabled": l.enabled,
                    "weight": l.weight,
                })
            })
            .collect();
        serde_json::json!({
            "masterEnabled": stack.master_enabled,
            "layers": layers,
        })
        .to_string()
    })
}
