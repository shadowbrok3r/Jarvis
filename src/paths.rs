//! Tiny helpers for path expansion shared across plugin and MCP layers.
//!
//! Previously [`crate::mcp::plugin::expand_home`] existed as a private copy;
//! both the pose library and the new MToon overrides plugin want the same
//! `~/...` handling, so it now lives here.

use std::path::{Path, PathBuf};

/// Expand a leading `~/` to `$HOME/`. Everything else (absolute paths, relative
/// paths, `$VAR`-style substitutions) is returned untouched — callers that need
/// richer expansion can layer their own logic on top.
pub fn expand_home(raw: impl AsRef<Path>) -> PathBuf {
    let raw = raw.as_ref();
    let Some(raw_str) = raw.to_str() else {
        return raw.to_path_buf();
    };
    if let Some(stripped) = raw_str.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(stripped);
        }
    }
    PathBuf::from(raw_str)
}
