//! Live VRM eye debug: replicates `bevy_vrm1` head→look-target yaw/pitch and range-mapped
//! values, plus the **actual** `Transform` rotation (Euler YXZ, degrees) on each eye bone.
//! Keep the range-map helpers in sync with `vendor/bevy_vrm1/src/vrm/look_at.rs` `apply_*_eye_bone`.

use bevy::math::EulerRot;
use bevy::prelude::*;
use bevy_vrm1::prelude::*;

/// Filled in [`PostUpdate`] after the VRM pipeline has applied look-at and expressions, so
/// [`Transform`] on the eye bones should match the rendered face (before spring-bone wobble).
#[derive(Resource, Clone, Debug)]
pub struct VrmEyeLookatDebug {
    /// At least one VRM + look-at + humanoid eye bones was found and sampled.
    pub ready: bool,
    /// `LookAt` uses expression type — `bevy_vrm1` does not run bone look-at; values may be idle.
    pub vrm_uses_expression_lookat: bool,
    /// `Target` = driven by entity (HA gaze); `Cursor` = mouse; `n/a` = missing.
    pub look_mode: &'static str,
    /// Head look-at **space** yaw (°) and pitch (°) — same as `bevy_vrm1::vrm::look_at::calc_yaw_pitch` **before** VRM range map. `NaN` when in cursor mode (not recomputed here).
    pub yaw_head_deg: f32,
    pub pitch_head_deg: f32,
    /// After VRM `RangeMap` (same numerical yaw/pitch passed into the eye YXZ `to_eye_rotation` path).
    pub left_mapped_yaw_deg: f32,
    pub left_mapped_pitch_deg: f32,
    pub right_mapped_yaw_deg: f32,
    pub right_mapped_pitch_deg: f32,
    /// **Actual** `Transform::rotation` on the bone, local to parent. Euler YXZ, degrees, **(Y, X, Z)** order to match `EulerRot::YXZ`.
    pub left_eye_euler_yxz_deg: Vec3,
    pub right_eye_euler_yxz_deg: Vec3,
    /// VRM0 look-at `RangeMap` **input** degrees — if `|yaw_head|` is usually above the horizontal
    /// in_max, the eye will sit at the rail (full L/R) most of the time.
    pub range_h_outer_in_deg: f32,
    pub range_h_inner_in_deg: f32,
    pub range_v_down_in_deg: f32,
    pub range_v_up_in_deg: f32,
    /// If look-at is `Target`, the target world position used for the angles above.
    pub target_world: Option<Vec3>,
}

impl Default for VrmEyeLookatDebug {
    fn default() -> Self {
        Self {
            ready: false,
            vrm_uses_expression_lookat: false,
            look_mode: "n/a",
            yaw_head_deg: 0.0,
            pitch_head_deg: 0.0,
            left_mapped_yaw_deg: 0.0,
            left_mapped_pitch_deg: 0.0,
            right_mapped_yaw_deg: 0.0,
            right_mapped_pitch_deg: 0.0,
            left_eye_euler_yxz_deg: Vec3::ZERO,
            right_eye_euler_yxz_deg: Vec3::ZERO,
            range_h_outer_in_deg: 0.0,
            range_h_inner_in_deg: 0.0,
            range_v_down_in_deg: 0.0,
            range_v_up_in_deg: 0.0,
            target_world: None,
        }
    }
}

/// Same construction as `bevy_vrm1::vrm::look_at::track_looking_target` (lines ~87–91).
fn look_at_space_from_head(
    head_gtf: &GlobalTransform,
    head_tf: &Transform,
    offset_from_head: Vec3,
) -> GlobalTransform {
    let look_at_space = GlobalTransform::default();
    let mut look_at_space_tf = look_at_space.reparented_to(head_gtf);
    look_at_space_tf.translation = offset_from_head;
    look_at_space_tf.rotation = head_tf.rotation.inverse();
    head_gtf.mul_transform(look_at_space_tf)
}

/// Copy of `bevy_vrm1::vrm::look_at::calc_yaw_pitch`.
fn calc_yaw_pitch(look_at_space: &GlobalTransform, target: Vec3) -> (f32, f32) {
    let local_target = look_at_space.to_matrix().inverse().transform_point3(target);
    let z = local_target.dot(Vec3::Z);
    let x = local_target.dot(Vec3::X);
    let yaw = (x.atan2(z)).to_degrees();
    let xz = (x * x + z * z).sqrt();
    let y = local_target.dot(Vec3::Y);
    let pitch = (-y.atan2(xz)).to_degrees();
    (yaw, pitch)
}

fn safe_div(n: f32, d: f32) -> f32 {
    if d.abs() < 1.0e-6 {
        0.0
    } else {
        n / d
    }
}

fn map_yaw_left_eye(p: &LookAtProperties, yaw_deg: f32) -> f32 {
    let o = p.range_map_horizontal_outer;
    let i = p.range_map_horizontal_inner;
    if yaw_deg > 0.0 {
        yaw_deg.min(o.input_max_value) * safe_div(o.output_scale, o.input_max_value)
    } else {
        -(yaw_deg.abs().min(i.input_max_value) * safe_div(i.output_scale, i.input_max_value))
    }
}

fn map_yaw_right_eye(p: &LookAtProperties, yaw_deg: f32) -> f32 {
    let o = p.range_map_horizontal_outer;
    let i = p.range_map_horizontal_inner;
    if yaw_deg > 0.0 {
        yaw_deg.min(i.input_max_value) * safe_div(i.output_scale, i.input_max_value)
    } else {
        -(yaw_deg.abs().min(o.input_max_value) * safe_div(o.output_scale, o.input_max_value))
    }
}

fn map_pitch_eye(p: &LookAtProperties, pitch_deg: f32) -> f32 {
    if pitch_deg > 0.0 {
        let d = p.range_map_vertical_down;
        pitch_deg
            .min(d.input_max_value)
            * safe_div(d.output_scale, d.input_max_value)
    } else {
        let u = p.range_map_vertical_up;
        -(pitch_deg
            .abs()
            .min(u.input_max_value)
            * safe_div(u.output_scale, u.input_max_value))
    }
}

/// `EulerRot::YXZ` → (Y, X, Z) in degrees, stored as `Vec3(y, x, z)`.
fn quat_euler_yxz_yxz_deg(q: Quat) -> Vec3 {
    let (y, x, z) = q.to_euler(EulerRot::YXZ);
    Vec3::new(
        y.to_degrees(),
        x.to_degrees(),
        z.to_degrees(),
    )
}

/// Run **after** look-at and expressions and their transform propagations so the bone `Transform` matches
/// what the shader / next frame will use. See VRM `vrm::VrmPlugin` schedule in `bevy_vrm1`.
pub fn update_vrm_eye_lookat_debug(
    mut res: ResMut<VrmEyeLookatDebug>,
    vrm: Query<(
        &LookAt,
        &LookAtProperties,
        &HeadBoneEntity,
        &LeftEyeBoneEntity,
        &RightEyeBoneEntity,
    ), With<Vrm>>,
    gtf: Query<&GlobalTransform>,
    tf: Query<&Transform>,
) {
    *res = VrmEyeLookatDebug::default();

    let Ok((look_at, props, head, le, re)) = vrm.single() else {
        return;
    };

    if props.r#type == LookAtType::Expression {
        res.vrm_uses_expression_lookat = true;
        res.ready = true;
        return;
    }

    res.range_h_outer_in_deg = props.range_map_horizontal_outer.input_max_value;
    res.range_h_inner_in_deg = props.range_map_horizontal_inner.input_max_value;
    res.range_v_down_in_deg = props.range_map_vertical_down.input_max_value;
    res.range_v_up_in_deg = props.range_map_vertical_up.input_max_value;

    let Ok(head_gtf) = gtf.get(head.0) else {
        return;
    };
    let Ok(head_tf) = tf.get(head.0) else {
        return;
    };
    let offset = Vec3::from(props.offset_from_head_bone);
    let look_at_space = look_at_space_from_head(head_gtf, head_tf, offset);

    let (yaw_h, pitch_h) = match *look_at {
        LookAt::Cursor => {
            res.look_mode = "Cursor";
            (f32::NAN, f32::NAN)
        }
        LookAt::Target(te) => {
            res.look_mode = "Target";
            if let Ok(tgt) = gtf.get(te) {
                let p = tgt.translation();
                res.target_world = Some(p);
                calc_yaw_pitch(&look_at_space, p)
            } else {
                (0.0, 0.0)
            }
        }
    };

    res.yaw_head_deg = yaw_h;
    res.pitch_head_deg = pitch_h;

    if !yaw_h.is_nan() {
        res.left_mapped_yaw_deg = map_yaw_left_eye(props, yaw_h);
        res.left_mapped_pitch_deg = map_pitch_eye(props, pitch_h);
        res.right_mapped_yaw_deg = map_yaw_right_eye(props, yaw_h);
        res.right_mapped_pitch_deg = map_pitch_eye(props, pitch_h);
    }

    if let Ok(le_tf) = tf.get(le.0) {
        res.left_eye_euler_yxz_deg = quat_euler_yxz_yxz_deg(le_tf.rotation);
    }
    if let Ok(re_tf) = tf.get(re.0) {
        res.right_eye_euler_yxz_deg = quat_euler_yxz_yxz_deg(re_tf.rotation);
    }

    res.ready = true;
}
