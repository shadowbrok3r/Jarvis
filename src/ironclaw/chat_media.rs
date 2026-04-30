//! Extract `ImageData` from assistant markdown / `data:` URLs for gateway-synced chat UIs.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use once_cell::sync::Lazy;
use regex::Regex;

use super::types::ImageData;

/// `![](data:image/png;base64,...)`
static RE_MD_IMG: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"!\[[^\]]*\]\(\s*(data:image/(?:png|jpeg|jpg|webp|gif);base64,([A-Za-z0-9+/=]+))\s*\)",
    )
    .expect("regex")
});

/// Bare `data:image/...;base64,...` (not inside markdown parens we already stripped).
static RE_BARE_DATA: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"data:image/(?:png|jpeg|jpg|webp|gif);base64,[A-Za-z0-9+/=]+").expect("regex")
});

/// Parse `data:image/…;base64,…` into [`ImageData`] when base64 decodes.
pub fn image_data_from_data_url(data_url: &str) -> Option<ImageData> {
    let s = data_url.trim();
    let rest = s.strip_prefix("data:")?;
    let (mime_b64, b64) = rest.split_once(";base64,")?;
    if mime_b64.is_empty() || b64.is_empty() {
        return None;
    }
    let data = b64.to_string();
    B64.decode(data.as_bytes()).ok()?;
    Some(ImageData {
        media_type: mime_b64.to_string(),
        data,
    })
}

/// Remove markdown images and bare data-URL images; collect decoded metadata (base64 as on the wire).
pub fn strip_inline_images_from_assistant_text(text: &str) -> (String, Vec<ImageData>) {
    let mut images: Vec<ImageData> = Vec::new();
    let mut out = text.to_string();
    for caps in RE_MD_IMG.captures_iter(text) {
        let Some(full) = caps.get(1) else { continue };
        if let Some(img) = image_data_from_data_url(full.as_str()) {
            images.push(img);
        }
    }
    out = RE_MD_IMG.replace_all(&out, "").to_string();
    for m in RE_BARE_DATA.find_iter(&out) {
        if let Some(img) = image_data_from_data_url(m.as_str()) {
            images.push(img);
        }
    }
    out = RE_BARE_DATA.replace_all(&out, "").to_string();
    (out.trim().to_string(), images)
}
