//! List and resolve VRM paths under `assets/models/` (asset paths like `models/foo.vrm`).
//!
//! Shared by MCP `list_models` / `load_vrm` and the in-app Avatar debug window.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

/// Project `assets/` directory (cwd-relative).
pub fn assets_root() -> PathBuf {
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("assets")
}

pub fn models_dir() -> PathBuf {
    assets_root().join("models")
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelEntry {
    pub basename: String,
    /// Path relative to `assets/` for `AssetServer::load` / `[avatar].model_path`.
    pub asset_path: String,
}

/// Sorted `*.vrm` in `assets/models/` (non-recursive). Optional case-insensitive substring filter on basename.
pub fn list_vrm_models(filter: Option<&str>) -> Result<Vec<ModelEntry>, String> {
    let dir = models_dir();
    if !dir.is_dir() {
        return Err(format!(
            "models directory does not exist: {}",
            dir.display()
        ));
    }
    let needle = filter.map(|s| s.to_ascii_lowercase());
    let mut out: Vec<ModelEntry> = fs::read_dir(&dir)
        .map_err(|e| format!("read_dir {}: {e}", dir.display()))?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let path = e.path();
            if !path.is_file() {
                return None;
            }
            let name = path.file_name()?.to_string_lossy().into_owned();
            if !name.to_ascii_lowercase().ends_with(".vrm") {
                return None;
            }
            if let Some(ref n) = needle {
                if !name.to_ascii_lowercase().contains(n) {
                    return None;
                }
            }
            Some(ModelEntry {
                basename: name.clone(),
                asset_path: format!("models/{name}"),
            })
        })
        .collect();
    out.sort_by(|a, b| a.basename.to_ascii_lowercase().cmp(&b.basename.to_ascii_lowercase()));
    Ok(out)
}

fn canonical_under(parent: &Path, child: &Path) -> Result<PathBuf, String> {
    let pc = parent.canonicalize().map_err(|e| {
        format!(
            "canonicalize {}: {e} — is the path readable?",
            parent.display()
        )
    })?;
    let cc = child.canonicalize().map_err(|e| {
        format!(
            "canonicalize {}: {e}",
            child.display()
        )
    })?;
    if !cc.starts_with(&pc) {
        return Err(format!(
            "path escapes allowed directory: {} not under {}",
            cc.display(),
            pc.display()
        ));
    }
    Ok(cc)
}

/// Resolve MCP `load_vrm` argument to an `assets/`-relative path (`models/…`).
///
/// Accepts:
/// - Basename only: `foo.vrm` under `assets/models/`
/// - Asset-style: `models/foo.vrm` or `models\\foo.vrm`
/// - Leading `./` or optional `assets/` prefix is stripped before checks.
pub fn resolve_vrm_load_argument(arg: &str) -> Result<String, String> {
    let arg = arg.trim();
    if arg.is_empty() {
        return Err("empty path".into());
    }
    if !arg.to_ascii_lowercase().ends_with(".vrm") {
        return Err("path must end with .vrm".into());
    }

    let assets = assets_root();
    let models = models_dir();

    let rel_models_path = if arg.contains('/') || arg.contains('\\') {
        let mut s = arg.replace('\\', "/");
        while s.starts_with("./") {
            s = s[2..].to_string();
        }
        if let Some(rest) = s.strip_prefix("assets/") {
            s = rest.to_string();
        }
        if !s.starts_with("models/") {
            return Err(format!(
                "expected path under models/, got {s:?} — use models/name.vrm or a basename only"
            ));
        }
        s
    } else {
        format!("models/{arg}")
    };

    let full = assets.join(&rel_models_path);
    if !full.is_file() {
        return Err(format!("VRM file not found: {}", full.display()));
    }
    let _ = canonical_under(&models, &full)?;
    Ok(rel_models_path)
}
