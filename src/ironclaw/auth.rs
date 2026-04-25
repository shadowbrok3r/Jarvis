//! `module:authenticate` / `module:announce` payloads (serialize into `EnvelopeBody`).

use serde_json::{json, Value};

/// Build `data` object for `module:authenticate`.
#[must_use]
pub fn authenticate_data(token: &str) -> Value {
    json!({ "token": token })
}

/// Build `data` object for `module:announce`.
#[must_use]
pub fn announce_data(name: &str, identity: Value) -> Value {
    json!({ "name": name, "identity": identity })
}
