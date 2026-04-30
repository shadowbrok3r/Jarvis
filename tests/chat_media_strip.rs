use jarvis_avatar::ironclaw::chat_media::{
    image_data_from_data_url, strip_inline_images_from_assistant_text,
};

#[test]
fn data_url_roundtrip() {
    let raw = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
    let img = image_data_from_data_url(raw).expect("parse");
    assert_eq!(img.media_type, "image/png");
    assert!(!img.data.is_empty());
}

#[test]
fn strip_markdown_image() {
    let b64 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
    let s = format!("Hello ![](data:image/png;base64,{b64}) world");
    let (text, imgs) = strip_inline_images_from_assistant_text(&s);
    assert_eq!(text, "Hello  world");
    assert_eq!(imgs.len(), 1);
    assert_eq!(imgs[0].media_type, "image/png");
}
