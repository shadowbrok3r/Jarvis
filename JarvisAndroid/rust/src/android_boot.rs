//! Process bootstrap that runs before the Bevy `App` is built.

use std::path::{Path, PathBuf};

/// Paths resolved from the running `NativeActivity`.
pub struct AndroidCtx {
    /// App-internal storage (`/data/data/<pkg>/files`).
    pub internal_dir: PathBuf,
    /// Where extracted + hub-downloaded assets live.
    pub assets_dir: PathBuf,
}

pub fn init() -> AndroidCtx {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("jarvis"),
    );

    // rustls 0.23 refuses to auto-detect when more than one provider could be
    // linked. Desktop installs aws-lc-rs in `main`; Android links `ring`.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let internal_dir = bevy::android::ANDROID_APP
        .get()
        .and_then(|app| app.internal_data_path())
        .unwrap_or_else(|| PathBuf::from("/data/local/tmp/jarvis"));
    let assets_dir = internal_dir.join("assets");

    // Plugins resolve `config/...` relatively and `vrm_model_overrides_dir`
    // calls `current_dir()`, which is `/` on Android — every such read fails
    // with EACCES. Anchoring the cwd to internal storage fixes all of them at
    // once and leaves the desktop path untouched.
    let _ = std::fs::create_dir_all(internal_dir.join("config"));
    if let Err(e) = std::env::set_current_dir(&internal_dir) {
        log::error!("set_current_dir({}): {e}", internal_dir.display());
    }

    log::info!("internal_dir={}", internal_dir.display());
    AndroidCtx {
        internal_dir,
        assets_dir,
    }
}

/// The configured VRM, or the bundled one when that is not present.
///
/// `config/default.toml` names the desktop model; the APK carries whatever
/// `stage-assets.sh` selected, so a mismatch would otherwise be a hard asset
/// load failure at startup. Candidates come from the current bootstrap index,
/// never from a directory scan — a `.vrm` left over from an earlier staged set
/// would otherwise win.
///
/// Index entries are internal-storage-relative (`assets/models/x.vrm`); the
/// returned path is asset-root-relative (`models/x.vrm`) for the `AssetServer`.
pub fn resolve_model_path(ctx: &AndroidCtx, configured: &str, staged: &[String]) -> String {
    if ctx.assets_dir.join(configured).is_file() {
        return configured.to_string();
    }

    let found = staged.iter().filter_map(|rel| rel.strip_prefix("assets/")).find(|rel| {
        Path::new(rel).extension().is_some_and(|x| x.eq_ignore_ascii_case("vrm"))
    });

    match found {
        Some(rel) => {
            log::warn!("{configured} not bundled; using {rel}");
            rel.to_string()
        }
        None => {
            log::error!("no .vrm in the bootstrap index");
            configured.to_string()
        }
    }
}

/// Settings overlays, lowest precedence first.
///
/// 1. `<internal>/user.toml` — extracted from the APK. Carries the desktop
///    tuning with credential values blanked by `stage-assets.sh`.
/// 2. `<external>/user.toml` — `/sdcard/Android/data/<pkg>/files/user.toml`.
///    App-specific external storage is writable by `adb push` without root or a
///    debuggable build, so credentials can reach the phone without being baked
///    into the APK:
///
///    ```text
///    adb push config/user.toml \
///      /sdcard/Android/data/com.kingsofalchemy.jarvis/files/user.toml
///    ```
pub fn user_overlays(ctx: &AndroidCtx) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(s) = std::fs::read_to_string(ctx.internal_dir.join("user.toml")) {
        out.push(s);
    }
    if let Some(ext) = external_user_toml() {
        log::info!("external user.toml overlay applied");
        out.push(ext);
    }
    out
}

fn external_user_toml() -> Option<String> {
    let dir = bevy::android::ANDROID_APP.get()?.external_data_path()?;
    std::fs::read_to_string(dir.join("user.toml")).ok()
}

/// Clamps desktop-tuned graphics settings to what mobile GPUs accept.
///
/// `msaa_samples = 8` aborts the renderer on Adreno: `Depth32Float` supports
/// `[1, 2, 4]` there, and WebGPU only guarantees `[1, 4]`. Bevy treats the
/// resulting `create_texture` failure as fatal and quits.
pub fn clamp_graphics(settings: &mut jarvis_avatar::config::Settings) {
    let msaa = settings.graphics.msaa_samples;
    if msaa > 4 {
        log::warn!("msaa_samples {msaa} -> 4 (mobile depth formats cap at 4)");
        settings.graphics.msaa_samples = 4;
    }

    // Desktop authoring aid; on a phone the light gizmos are just crosses
    // floating over the avatar with no way to manipulate them.
    settings.light_rig.show_light_gizmos = false;
}

/// Repoints the pose library at internal storage.
///
/// `default.toml` points it at `~/.config/@proj-airi/...`, and Android has no
/// `$HOME`, so `expand_home` leaves a literal `~/` path that never resolves —
/// `avatar_defaults.idle_clip` then fails to find its clip by name.
pub fn redirect_pose_library(ctx: &AndroidCtx, settings: &mut jarvis_avatar::config::Settings) {
    let poses = ctx.assets_dir.join("poses");
    let anims = ctx.assets_dir.join("animations");
    let _ = std::fs::create_dir_all(&poses);
    let _ = std::fs::create_dir_all(&anims);
    settings.pose_library.poses_dir = poses.to_string_lossy().into_owned();
    settings.pose_library.animations_dir = anims.to_string_lossy().into_owned();
    log::info!("pose library -> {}", ctx.assets_dir.display());
}
