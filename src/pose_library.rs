//! Filesystem-backed pose and animation library.
//!
//! Mirrors the JSON format from the Node `pose-controller` server so the two
//! can share the same `~/.config/@proj-airi/.../poses` and `.../animations`
//! directories without re-encoding.
//!
//! Filename sanitation rule (same as `server.mjs`):
//! `name.replace(/[^a-z0-9_-]/gi, '_').toLowerCase() + '.json'`.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum LibraryError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("not found: {0}")]
    NotFound(String),
}

/// On-disk pose layout — matches the Node tool exactly (camelCase JSON keys).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PoseFile {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_category")]
    pub category: String,
    /// Bone name → `{ rotation: [x, y, z, w] }`.
    #[serde(default)]
    pub bones: HashMap<String, BoneRotation>,
    /// Expression name → 0..=1 weight.
    #[serde(default)]
    pub expressions: HashMap<String, f32>,
    #[serde(default = "default_transition")]
    pub transition_duration: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BoneRotation {
    /// Quaternion `[x, y, z, w]`.
    pub rotation: [f32; 4],
}

fn default_category() -> String {
    "general".to_string()
}
fn default_transition() -> f32 {
    0.4
}

/// One keyframe of a VRM animation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnimationFrame {
    pub bones: HashMap<String, BoneRotation>,
    /// Kimodo historically wrote `duration_ms` (snake); camelCase JSON uses `durationMs`.
    #[serde(default, alias = "duration_ms")]
    pub duration_ms: Option<f64>,
    /// Optional VRM expression weights (0..=1) for this keyframe. Used by
    /// animation layer clips (`anim_layers`); native JSON playback emits them
    /// when non-empty. Omitted keys in a sparse frame are not cleared on apply
    /// (see `POSE_GUIDE.md` — include explicit `0.0` to turn a morph off).
    #[serde(default)]
    pub expressions: HashMap<String, f32>,
}

/// On-disk animation layout (same JSON the Kimodo Python service emits).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnimationFile {
    pub name: String,
    #[serde(default)]
    pub prompt: String,
    #[serde(default = "default_fps")]
    pub fps: f64,
    #[serde(default)]
    pub frame_count: usize,
    pub frames: Vec<AnimationFrame>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub looping: Option<bool>,
    #[serde(default)]
    pub hold_duration: Option<f32>,
}

fn default_fps() -> f64 {
    30.0
}

/// Lightweight metadata for list endpoints.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnimationMeta {
    pub filename: String,
    pub name: String,
    pub category: String,
    pub looping: bool,
    pub hold_duration: f32,
    pub fps: f64,
    pub frame_count: usize,
}

/// File-backed library rooted at two configurable directories.
#[derive(Debug, Clone)]
pub struct PoseLibrary {
    pub poses_dir: PathBuf,
    pub animations_dir: PathBuf,
}

impl PoseLibrary {
    pub fn new(poses_dir: impl Into<PathBuf>, animations_dir: impl Into<PathBuf>) -> Self {
        Self {
            poses_dir: poses_dir.into(),
            animations_dir: animations_dir.into(),
        }
    }

    // ---------- poses -------------------------------------------------------

    pub fn load_all_poses(&self) -> Result<Vec<PoseFile>, LibraryError> {
        read_json_dir(&self.poses_dir)
    }

    pub fn find_pose(&self, name: &str) -> Result<Option<PoseFile>, LibraryError> {
        Ok(self
            .load_all_poses()?
            .into_iter()
            .find(|p| p.name == name))
    }

    /// Load a pose from disk for layer drivers and tooling.
    ///
    /// Resolution order:
    /// 1. If `key` ends with `.json` and `poses_dir/key` exists, load that file.
    /// 2. Otherwise match a pose whose [`PoseFile::name`] equals `key` (same as [`Self::find_pose`]).
    /// 3. Otherwise try `poses_dir/{slugify(key)}.json`.
    pub fn load_pose(&self, key: &str) -> Result<PoseFile, LibraryError> {
        let trimmed = key.trim();
        if trimmed.is_empty() {
            return Err(LibraryError::NotFound(key.to_string()));
        }
        if trimmed.ends_with(".json") {
            let path = self.poses_dir.join(trimmed);
            if path.is_file() {
                let body = fs::read_to_string(&path)?;
                return Ok(serde_json::from_str(&body)?);
            }
        }
        if let Some(p) = self.find_pose(trimmed)? {
            return Ok(p);
        }
        let slug_path = self.poses_dir.join(format!("{}.json", slugify(trimmed)));
        if slug_path.is_file() {
            let body = fs::read_to_string(&slug_path)?;
            return Ok(serde_json::from_str(&body)?);
        }
        Err(LibraryError::NotFound(trimmed.to_string()))
    }

    /// Like [`Self::load_pose`], but if the key is missing, scans all pose
    /// files for a matching display name or slug stem (for older layer-set
    /// blueprints).
    pub fn load_pose_loose(&self, key: &str) -> Result<PoseFile, LibraryError> {
        match self.load_pose(key) {
            Ok(p) => Ok(p),
            Err(LibraryError::NotFound(_)) => {
                let all = self.load_all_poses()?;
                let stem = key
                    .trim()
                    .strip_suffix(".json")
                    .unwrap_or(key.trim());
                all.into_iter()
                    .find(|p| {
                        slugify(&p.name) == stem
                            || p.name == key.trim()
                            || p.name == stem
                            || format!("{}.json", slugify(&p.name)) == key.trim()
                    })
                    .ok_or_else(|| LibraryError::NotFound(key.to_string()))
            }
            Err(e) => Err(e),
        }
    }

    pub fn save_pose(&self, pose: &PoseFile) -> Result<PathBuf, LibraryError> {
        fs::create_dir_all(&self.poses_dir)?;
        let path = self.poses_dir.join(format!("{}.json", slugify(&pose.name)));
        let body = serde_json::to_string_pretty(pose)?;
        fs::write(&path, body)?;
        Ok(path)
    }

    pub fn delete_pose(&self, name: &str) -> Result<(), LibraryError> {
        let path = self.poses_dir.join(format!("{}.json", slugify(name)));
        if !path.exists() {
            return Err(LibraryError::NotFound(name.to_string()));
        }
        fs::remove_file(path)?;
        Ok(())
    }

    pub fn rename_pose(&self, old: &str, new_name: &str) -> Result<(), LibraryError> {
        let mut pose = self
            .find_pose(old)?
            .ok_or_else(|| LibraryError::NotFound(old.to_string()))?;
        let old_path = self.poses_dir.join(format!("{}.json", slugify(old)));
        if old_path.exists() {
            let _ = fs::remove_file(&old_path);
        }
        pose.name = new_name.to_string();
        self.save_pose(&pose)?;
        Ok(())
    }

    pub fn update_pose_category(&self, name: &str, category: &str) -> Result<(), LibraryError> {
        let mut pose = self
            .find_pose(name)?
            .ok_or_else(|| LibraryError::NotFound(name.to_string()))?;
        pose.category = category.to_string();
        self.save_pose(&pose)?;
        Ok(())
    }

    // ---------- animations --------------------------------------------------

    pub fn list_animations(&self) -> Result<Vec<AnimationMeta>, LibraryError> {
        if !self.animations_dir.exists() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        let mut entries: Vec<PathBuf> = fs::read_dir(&self.animations_dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().map(|ext| ext == "json").unwrap_or(false))
            .collect();
        entries.sort();
        for path in entries {
            let filename = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            let name_stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            let content = match fs::read_to_string(&path) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let value: serde_json::Value = match serde_json::from_str(&content) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let category = value
                .get("category")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| {
                    if name_stem.starts_with("idle_") {
                        "idle".into()
                    } else if name_stem.starts_with("talk_") {
                        "talking".into()
                    } else {
                        "general".into()
                    }
                });
            let looping = value
                .get("looping")
                .and_then(|v| v.as_bool())
                .unwrap_or_else(|| name_stem.starts_with("idle_"));
            let hold_duration = value
                .get("holdDuration")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0) as f32;
            let fps = value.get("fps").and_then(|v| v.as_f64()).unwrap_or(30.0);
            let frame_count = value
                .get("frames")
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            let name = value
                .get("name")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .unwrap_or(name_stem);
            out.push(AnimationMeta {
                filename,
                name,
                category,
                looping,
                hold_duration,
                fps,
                frame_count,
            });
        }
        Ok(out)
    }

    pub fn load_animation(&self, filename: &str) -> Result<AnimationFile, LibraryError> {
        let path = self.animations_dir.join(filename);
        if !path.exists() {
            return Err(LibraryError::NotFound(filename.to_string()));
        }
        let body = fs::read_to_string(&path)?;
        let anim: AnimationFile = serde_json::from_str(&body)?;
        Ok(anim)
    }

    pub fn delete_animation(&self, filename: &str) -> Result<(), LibraryError> {
        let path = self.animations_dir.join(filename);
        if !path.exists() {
            return Err(LibraryError::NotFound(filename.to_string()));
        }
        fs::remove_file(path)?;
        Ok(())
    }

    pub fn rename_animation(
        &self,
        old_filename: &str,
        new_filename: &str,
    ) -> Result<(), LibraryError> {
        let old_path = self.animations_dir.join(old_filename);
        if !old_path.exists() {
            return Err(LibraryError::NotFound(old_filename.to_string()));
        }
        let body = fs::read_to_string(&old_path)?;
        let mut v: serde_json::Value = serde_json::from_str(&body)?;
        if let Some(obj) = v.as_object_mut() {
            obj.insert(
                "name".to_string(),
                serde_json::Value::String(
                    new_filename.trim_end_matches(".json").to_string(),
                ),
            );
        }
        let new_path = self.animations_dir.join(new_filename);
        fs::write(&new_path, serde_json::to_string_pretty(&v)?)?;
        if new_path != old_path {
            let _ = fs::remove_file(&old_path);
        }
        Ok(())
    }

    pub fn update_animation_metadata(
        &self,
        filename: &str,
        category: Option<String>,
        looping: Option<bool>,
        hold_duration: Option<f32>,
    ) -> Result<(), LibraryError> {
        let path = self.animations_dir.join(filename);
        if !path.exists() {
            return Err(LibraryError::NotFound(filename.to_string()));
        }
        let body = fs::read_to_string(&path)?;
        let mut v: serde_json::Value = serde_json::from_str(&body)?;
        if let Some(obj) = v.as_object_mut() {
            if let Some(c) = category {
                obj.insert("category".to_string(), serde_json::Value::String(c));
            }
            if let Some(l) = looping {
                obj.insert("looping".to_string(), serde_json::Value::Bool(l));
            }
            if let Some(h) = hold_duration {
                obj.insert(
                    "holdDuration".to_string(),
                    serde_json::Value::from(h as f64),
                );
            }
        }
        fs::write(&path, serde_json::to_string_pretty(&v)?)?;
        Ok(())
    }
}

fn read_json_dir<T: for<'de> Deserialize<'de>>(dir: &Path) -> Result<Vec<T>, LibraryError> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().map(|ext| ext != "json").unwrap_or(true) {
            continue;
        }
        match fs::read_to_string(&path) {
            Ok(body) => match serde_json::from_str::<T>(&body) {
                Ok(v) => out.push(v),
                Err(e) => {
                    tracing::warn!("skipping {:?}: {e}", path);
                }
            },
            Err(e) => tracing::warn!("skipping {:?}: {e}", path),
        }
    }
    Ok(out)
}

/// Match the JS regex `[^a-z0-9_-]/gi → _` then lowercase.
pub fn slugify(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn animation_frame_deserializes_expressions_and_duration_aliases() {
        let j = r#"{"bones":{"hips":{"rotation":[0.0,0.0,0.0,1.0]}},"duration_ms":33.3,"expressions":{"happy":0.5}}"#;
        let f: AnimationFrame = serde_json::from_str(j).unwrap();
        assert!((f.expressions["happy"] - 0.5).abs() < 1e-6);
        assert_eq!(f.duration_ms, Some(33.3));

        let j2 = r#"{"bones":{"hips":{"rotation":[0.0,0.0,0.0,1.0]}},"durationMs":40.0}"#;
        let f2: AnimationFrame = serde_json::from_str(j2).unwrap();
        assert_eq!(f2.duration_ms, Some(40.0));
    }

    #[test]
    fn slugify_matches_js_regex() {
        assert_eq!(slugify("My Cool Pose"), "my_cool_pose");
        assert_eq!(slugify("wave-hello_v2"), "wave-hello_v2");
        assert_eq!(slugify("ALLCAPS!?"), "allcaps__");
    }

    #[test]
    fn save_load_roundtrip() {
        let dir = tempdir().unwrap();
        let lib = PoseLibrary::new(dir.path().join("poses"), dir.path().join("anim"));
        let pose = PoseFile {
            name: "Wave".into(),
            description: "hi".into(),
            category: "greeting".into(),
            bones: HashMap::from([(
                "rightHand".to_string(),
                BoneRotation { rotation: [0.0, 0.1, 0.0, 0.995] },
            )]),
            expressions: HashMap::from([("happy".to_string(), 0.5)]),
            transition_duration: 0.5,
        };
        lib.save_pose(&pose).unwrap();
        let loaded = lib.find_pose("Wave").unwrap().unwrap();
        assert_eq!(loaded.name, "Wave");
        assert_eq!(loaded.category, "greeting");
        assert!((loaded.bones["rightHand"].rotation[3] - 0.995).abs() < 1e-6);

        lib.rename_pose("Wave", "Bigger Wave").unwrap();
        assert!(lib.find_pose("Wave").unwrap().is_none());
        assert!(lib.find_pose("Bigger Wave").unwrap().is_some());
        assert!(dir.path().join("poses/bigger_wave.json").exists());

        let via_slug = lib.load_pose("bigger_wave.json").unwrap();
        assert_eq!(via_slug.name, "Bigger Wave");
        let via_name = lib.load_pose("Bigger Wave").unwrap();
        assert_eq!(via_name.name, "Bigger Wave");

        lib.update_pose_category("Bigger Wave", "emotion").unwrap();
        assert_eq!(lib.find_pose("Bigger Wave").unwrap().unwrap().category, "emotion");

        lib.delete_pose("Bigger Wave").unwrap();
        assert!(lib.find_pose("Bigger Wave").unwrap().is_none());
    }
}
