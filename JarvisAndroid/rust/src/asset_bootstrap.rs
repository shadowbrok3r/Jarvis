//! First-run extraction of the APK's bundled files into internal storage.
//!
//! Bevy's `AndroidAssetReader` reads only from the APK and copies whole files
//! into RAM, which is untenable for `.vrm` payloads. Everything is extracted
//! once to a real directory that `FileAssetReader` then serves, and that same
//! directory is where hub-downloaded models land.
//!
//! `AAssetDir` does not enumerate subdirectories, so the staged tree carries an
//! `index.txt` of newline-separated paths **relative to internal storage** —
//! `assets/...` for Bevy and `config/...` for the plugin layer, which reads it
//! through the cwd anchored in `android_boot::init`.

use std::ffi::CString;
use std::io::Write;
use std::path::Path;

use crate::android_boot::AndroidCtx;

const INDEX_NAME: &str = "index.txt";

/// Extracts bundled assets and returns the index entries.
///
/// The marker is keyed on a hash of `index.txt` rather than the crate version,
/// so restaging a different bootstrap set re-extracts without a version bump.
pub fn extract_if_needed(ctx: &AndroidCtx) -> Vec<String> {
    if let Err(e) = std::fs::create_dir_all(&ctx.internal_dir) {
        log::error!("create internal dir: {e}");
        return Vec::new();
    }

    let Some(app) = bevy::android::ANDROID_APP.get() else {
        log::error!("ANDROID_APP unset; skipping asset extraction");
        return Vec::new();
    };
    let mgr = app.asset_manager();

    let Some(raw_index) = read_asset(&mgr, INDEX_NAME) else {
        log::warn!("no {INDEX_NAME} in APK assets; nothing to extract");
        return Vec::new();
    };
    let index = String::from_utf8_lossy(&raw_index).into_owned();
    // `#` lines carry the content stamp, which exists so the marker below
    // changes when a staged file's *contents* change and not just the list.
    let entries: Vec<String> = index
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
        .collect();

    let marker = ctx
        .internal_dir
        .join(format!(".bootstrap-{:016x}", fnv1a(&raw_index)));
    if marker.exists() {
        log::info!("already extracted ({} entries)", entries.len());
        return entries;
    }

    let mut extracted = 0usize;
    for rel in &entries {
        if copy_asset(&mgr, rel, &ctx.internal_dir.join(rel)) {
            extracted += 1;
        }
    }

    // Drop markers from earlier bootstrap sets so they cannot mask a later one.
    if let Ok(dir) = std::fs::read_dir(&ctx.internal_dir) {
        for old in dir.flatten().map(|e| e.path()).filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(".bootstrap-"))
        }) {
            let _ = std::fs::remove_file(old);
        }
    }
    let _ = std::fs::File::create(&marker);
    log::info!(
        "extracted {extracted}/{} files to {}",
        entries.len(),
        ctx.internal_dir.display()
    );
    entries
}

fn read_asset(mgr: &ndk::asset::AssetManager, rel: &str) -> Option<Vec<u8>> {
    let c = CString::new(rel).ok()?;
    let mut asset = mgr.open(&c)?;
    asset.buffer().ok().map(<[u8]>::to_vec)
}

/// Streams one APK asset to disk. `Asset: io::Read`, so a 126 MB VRM never
/// needs a 126 MB allocation the way `Asset::buffer` would.
fn copy_asset(mgr: &ndk::asset::AssetManager, rel: &str, dst: &Path) -> bool {
    let Ok(c) = CString::new(rel) else {
        log::error!("bad asset name: {rel}");
        return false;
    };
    let Some(mut asset) = mgr.open(&c) else {
        log::warn!("missing APK asset: {rel}");
        return false;
    };
    if let Some(parent) = dst.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            log::error!("mkdir {}: {e}", parent.display());
            return false;
        }
    }
    let mut out = match std::fs::File::create(dst) {
        Ok(f) => std::io::BufWriter::new(f),
        Err(e) => {
            log::error!("create {}: {e}", dst.display());
            return false;
        }
    };
    match std::io::copy(&mut asset, &mut out).and_then(|_| out.flush()) {
        Ok(()) => true,
        Err(e) => {
            log::error!("write {}: {e}", dst.display());
            false
        }
    }
}

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x100_0000_01b3)
    })
}
