//! Dedicated Chat window: collapsible thread sidebar, bubble-style markdown
//! transcript, compose bar with file-picker + drag-and-drop attachments.
//!
//! Rendering uses `egui_commonmark` for CommonMark + GFM subset (tables,
//! tasklists, strikethrough). Attachments piggyback on
//! [`jarvis_avatar::ironclaw::client::GatewayClient::attach_file`] so they
//! always hit the gateway as canonical `ImageData` regardless of how the user
//! added them (dialog vs drag-and-drop).

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use bevy::prelude::*;
use bevy_egui::egui::{Button, Layout, TopBottomPanel, Vec2, Widget};
use bevy_egui::{EguiContexts, egui};
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};

use jarvis_avatar::act::strip_act_delay;
use jarvis_avatar::config::Settings;
use jarvis_avatar::ironclaw::client::GatewayClient;
use jarvis_avatar::ironclaw::types::{ImageData, ThreadInfo};

use super::DebugUiState;
use crate::plugins::home_assistant_events::AiriHaEventQueue;
use crate::plugins::ironclaw_chat::{
    ChatState, GatewayClientHandle, LocalUserEchoMessage, TranscriptLine, TranscriptRole,
};

/// One attached image waiting to be sent with the next message.
#[derive(Debug, Clone)]
pub struct PendingAttachment {
    /// Display label — file name or a generated "drop-N.png".
    pub name: String,
    /// Canonical base64-encoded payload the gateway expects.
    pub image: ImageData,
}

#[derive(Default)]
pub struct ChatUiState {
    pub input: String,
    pub pending: Vec<PendingAttachment>,
    pub sidebar_collapsed: bool,
    pub markdown_cache: CommonMarkCache,
}

pub fn draw_chat_window(
    mut contexts: EguiContexts,
    mut settings: ResMut<Settings>,
    mut state: ResMut<DebugUiState>,
    chat: Option<Res<ChatState>>,
    gateway: Option<Res<GatewayClientHandle>>,
    mut airi_events: Option<ResMut<AiriHaEventQueue>>,
    mut echo: MessageWriter<LocalUserEchoMessage>,
) {
    if !settings.ui.show_chat {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };
    let mut open = settings.ui.show_chat;
    egui::Window::new("Chat")
        .default_size([760.0, 560.0])
        .resizable(true)
        .open(&mut open)
        .show(ctx, |ui| match chat.as_deref() {
            None => {
                ui.vertical_centered(|ui| {
                    ui.add_space(60.0);
                    ui.label("chat plugin not running");
                });
            }
            Some(chat) => {
                // Consume drag-and-drop files first so the compose bar sees the
                // updated `state.chat.pending` list in the same frame.
                ingest_dropped_files(ui.ctx(), &mut state);

                TopBottomPanel::top("chat_top_panel").show_inside(ui, |ui| {
                    ui.with_layout(Layout::left_to_right(egui::Align::Min), |ui| {
                        if ui
                            .button(if state.chat.sidebar_collapsed {
                                "»"
                            } else {
                                "«"
                            })
                            .on_hover_text("collapse / expand sidebar")
                            .clicked()
                        {
                            state.chat.sidebar_collapsed = !state.chat.sidebar_collapsed;
                        }
                        if ui.button("+ New Chat").clicked() {
                            if let Some(g) = gateway.as_ref() {
                                g.new_thread();
                            }
                        }
                    });
                });

                if !state.chat.sidebar_collapsed {
                    egui::SidePanel::left("chat_thread_sidebar")
                        .resizable(true)
                        .default_width(210.0)
                        .min_width(44.0)
                        .show_inside(ui, |ui| {
                            thread_sidebar(ui, &mut state, chat, gateway.as_deref())
                        });
                }

                egui::TopBottomPanel::bottom("chat_compose_bar")
                    .resizable(false)
                    .min_height(96.0)
                    .show_inside(ui, |ui| {
                        compose_bar(
                            ui,
                            &mut state,
                            chat,
                            gateway.as_deref(),
                            airi_events.as_deref_mut(),
                            &mut echo,
                        )
                    });

                egui::CentralPanel::default()
                    .show_inside(ui, |ui| transcript(ui, &mut state.chat, chat));

                // Drag-and-drop overlay hint.
                if ui.ctx().input(|i| !i.raw.hovered_files.is_empty()) {
                    draw_drop_overlay(ui.ctx());
                }
            }
        });
    settings.ui.show_chat = open;
}

// ---------- Side panel --------------------------------------------------------

fn thread_sidebar(
    ui: &mut egui::Ui,
    state: &mut DebugUiState,
    chat: &ChatState,
    gateway: Option<&GatewayClientHandle>,
) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Threads").strong());
        ui.with_layout(Layout::right_to_left(egui::Align::Max), |ui| {
            if ui
                .add_enabled(gateway.is_some(), egui::Button::new("⟳"))
                .on_hover_text("refresh thread list")
                .clicked()
            {
                if let Some(g) = gateway {
                    g.refresh_threads();
                }
            }
        });
    });

    ui.separator();

    egui::ScrollArea::vertical()
        .id_salt("chat_thread_list")
        .show(ui, |ui| {
            if chat.threads.is_empty() {
                ui.label(egui::RichText::new("(no threads yet)").italics());
            } else {
                for thread in &chat.threads {
                    render_thread_row(ui, state, chat, gateway, thread);
                }
            }
        });

    ui.separator();
    connection_indicator(ui, chat);
}

fn render_thread_row(
    ui: &mut egui::Ui,
    _state: &mut DebugUiState,
    chat: &ChatState,
    gateway: Option<&GatewayClientHandle>,
    thread: &ThreadInfo,
) {
    let is_active = chat.active_thread.as_deref() == Some(thread.id.as_str());
    let title = thread.title.clone().unwrap_or_else(|| short_id(&thread.id));
    let label = format!("{title}\n  {} turns", thread.turn_count);
    if Button::selectable(is_active, egui::RichText::new(label).monospace())
        .truncate()
        .min_size(Vec2::new(
            ui.available_size().x / 1.1,
            ui.text_style_height(&egui::TextStyle::Body),
        ))
        .ui(ui)
        .clicked()
    {
        if let Some(g) = gateway {
            g.set_active_thread(thread.id.clone());
        }
    }
}

fn connection_indicator(ui: &mut egui::Ui, chat: &ChatState) {
    let (color, text) = if !chat.has_bearer {
        (
            egui::Color32::from_rgb(220, 90, 90),
            "no bearer token".to_string(),
        )
    } else if chat.last_error.is_some() {
        (
            egui::Color32::from_rgb(220, 90, 90),
            chat.last_error.clone().unwrap_or_default(),
        )
    } else {
        (
            egui::Color32::from_rgb(80, 200, 120),
            chat.last_status.clone().unwrap_or_else(|| "idle".into()),
        )
    };
    ui.horizontal_wrapped(|ui| {
        ui.colored_label(color, "●");
        ui.small(text);
    });
    ui.small(&chat.base_url);
}

// ---------- Transcript --------------------------------------------------------

/// Decode gateway [`ImageData`] (base64) and show under the markdown bubble.
fn render_transcript_inline_images(ui: &mut egui::Ui, line_idx: usize, images: &[ImageData]) {
    const MAX_H: f32 = 240.0;
    for (img_i, im) in images.iter().enumerate() {
        let Ok(raw) = B64.decode(im.data.as_bytes()) else {
            continue;
        };
        let Ok(img) = image::load_from_memory(&raw) else {
            continue;
        };
        let rgba = img.to_rgba8();
        let w = rgba.width() as usize;
        let h = rgba.height() as usize;
        if w == 0 || h == 0 {
            continue;
        }
        let color = egui::ColorImage::from_rgba_unmultiplied([w, h], rgba.as_raw());
        let name = format!("gw_img_l{line_idx}_i{img_i}_b{}", im.data.len());
        let tex = ui
            .ctx()
            .load_texture(name, color, egui::TextureOptions::LINEAR);
        let mut sz = tex.size_vec2();
        if sz.y > MAX_H {
            sz *= MAX_H / sz.y;
        }
        ui.image((tex.id(), sz));
    }
}

fn transcript(ui: &mut egui::Ui, chat_ui: &mut ChatUiState, chat: &ChatState) {
    egui::ScrollArea::vertical()
        .id_salt("chat_transcript")
        .auto_shrink([false; 2])
        .stick_to_bottom(true)
        .show(ui, |ui| {
            for (idx, line) in chat.transcript.iter().enumerate() {
                render_bubble(
                    ui,
                    &mut chat_ui.markdown_cache,
                    idx,
                    line,
                    &mut chat_ui.input,
                );
            }
            if !chat.streaming_buffer.is_empty() {
                render_streaming_bubble(
                    ui,
                    &mut chat_ui.markdown_cache,
                    &chat.streaming_buffer,
                    chat.thinking.as_deref(),
                    &mut chat_ui.input,
                );
            }
        });
}

fn render_bubble(
    ui: &mut egui::Ui,
    cache: &mut CommonMarkCache,
    idx: usize,
    line: &TranscriptLine,
    compose_input: &mut String,
) {
    let (align, label, accent) = bubble_style(line.role);
    // ACT / DELAY tokens belong to the dispatcher, not the reader —
    // scrub them so `[ACT emotion="sensual"]` doesn't clutter bubbles.
    let body = strip_act_delay(&line.text);
    let body = if body.trim().is_empty() {
        " "
    } else {
        body.as_ref()
    };
    ui.with_layout(egui::Layout::top_down(align), |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.colored_label(accent, label);
        });
        if line.role == TranscriptRole::Assistant {
            if let Some(t) = line
                .thinking
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                render_thinking_collapsible(ui, format!("chat-think-{idx}"), t);
            }
            if let Some(ref json) = line.tool_calls_json {
                if !json.trim().is_empty() {
                    render_tool_calls_collapsible(ui, format!("chat-tools-{idx}"), json);
                }
            }
        }
        egui::Frame::group(ui.style())
            .fill(bubble_fill(line.role, ui.visuals()))
            .show(ui, |ui| {
                ui.set_max_width((ui.available_width() - 40.0).max(200.0));
                CommonMarkViewer::new().show(ui, cache, body);
            });
        if !line.images.is_empty() {
            ui.horizontal_wrapped(|ui| {
                render_transcript_inline_images(ui, idx, &line.images);
            });
        }
        if line.role == TranscriptRole::Assistant {
            render_suggestion_chips(ui, compose_input, &line.suggestions, idx);
        }
        ui.add_space(6.0);
    });
}

fn render_streaming_bubble(
    ui: &mut egui::Ui,
    cache: &mut CommonMarkCache,
    body: &str,
    thinking: Option<&str>,
    compose_input: &mut String,
) {
    let (display_raw, suggestions, tool_calls_json) =
        TranscriptLine::parse_raw_assistant_bubble(body);
    let stripped = strip_act_delay(&display_raw);
    let display = if stripped.trim().is_empty() {
        " "
    } else {
        stripped.as_ref()
    };
    ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.colored_label(egui::Color32::from_rgb(140, 200, 140), "jarvis · typing…");
        });
        if let Some(t) = thinking.map(str::trim).filter(|s| !s.is_empty()) {
            render_thinking_collapsible(ui, "chat-think-streaming", t);
        }
        if let Some(ref json) = tool_calls_json {
            if !json.trim().is_empty() {
                render_tool_calls_collapsible(ui, "chat-tools-streaming", json);
            }
        }
        egui::Frame::group(ui.style())
            .fill(bubble_fill(TranscriptRole::Assistant, ui.visuals()))
            .show(ui, |ui| {
                ui.set_max_width((ui.available_width() - 40.0).max(200.0));
                CommonMarkViewer::new().show(ui, cache, display);
            });
        render_suggestion_chips(ui, compose_input, &suggestions, usize::MAX);
        ui.add_space(6.0);
    });
}

/// Collapsible reasoning / model thinking (plain text, above the reply bubble).
fn render_thinking_collapsible(ui: &mut egui::Ui, id_salt: impl std::hash::Hash, text: &str) {
    egui::CollapsingHeader::new(egui::RichText::new("Thinking").small())
        .id_salt(id_salt)
        .default_open(false)
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .max_height(220.0)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(text)
                                .monospace()
                                .line_height(Some(15.0)),
                        )
                        .wrap(),
                    );
                });
        });
    ui.add_space(4.0);
}

/// Collapsible pretty-printed `tool_calls` JSON from a structured assistant envelope.
fn render_tool_calls_collapsible(ui: &mut egui::Ui, id_salt: impl std::hash::Hash, json: &str) {
    egui::CollapsingHeader::new(egui::RichText::new("Tool calls").small())
        .id_salt(id_salt)
        .default_open(false)
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .max_height(220.0)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(json)
                                .monospace()
                                .line_height(Some(15.0)),
                        )
                        .wrap(),
                    );
                });
        });
    ui.add_space(4.0);
}

/// Clickable suggestion “links” that append the text into the compose field.
fn render_suggestion_chips(
    ui: &mut egui::Ui,
    compose_input: &mut String,
    suggestions: &[String],
    bubble_idx: usize,
) {
    if suggestions.is_empty() {
        return;
    }
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new("Suggestions").small().strong());
        ui.add_space(6.0);
        for (si, s) in suggestions.iter().enumerate() {
            let label = egui::RichText::new(s.as_str())
                .small()
                .color(egui::Color32::from_rgb(140, 190, 255));
            ui.push_id((bubble_idx, si), |ui| {
                if ui.link(label).clicked() {
                    append_suggestion_to_input(compose_input, s);
                }
            });
            ui.add_space(8.0);
        }
    });
}

fn append_suggestion_to_input(input: &mut String, suggestion: &str) {
    let s = suggestion.trim();
    if s.is_empty() {
        return;
    }
    if !input.is_empty() && !input.ends_with(char::is_whitespace) {
        input.push(' ');
    }
    input.push_str(s);
}

fn bubble_style(role: TranscriptRole) -> (egui::Align, &'static str, egui::Color32) {
    match role {
        TranscriptRole::User => (
            egui::Align::Max,
            "you",
            egui::Color32::from_rgb(180, 220, 255),
        ),
        TranscriptRole::Assistant => (
            egui::Align::Min,
            "jarvis",
            egui::Color32::from_rgb(220, 220, 220),
        ),
        TranscriptRole::Tool => (
            egui::Align::Min,
            "tool",
            egui::Color32::from_rgb(220, 200, 140),
        ),
        TranscriptRole::System => (
            egui::Align::Min,
            "sys",
            egui::Color32::from_rgb(160, 160, 160),
        ),
    }
}

fn bubble_fill(role: TranscriptRole, visuals: &egui::Visuals) -> egui::Color32 {
    let base = visuals.extreme_bg_color;
    match role {
        TranscriptRole::User => blend(base, egui::Color32::from_rgb(40, 70, 120), 0.35),
        TranscriptRole::Assistant => blend(base, egui::Color32::from_rgb(50, 50, 60), 0.35),
        TranscriptRole::Tool => blend(base, egui::Color32::from_rgb(90, 70, 30), 0.35),
        TranscriptRole::System => blend(base, egui::Color32::from_rgb(40, 40, 40), 0.35),
    }
}

fn blend(a: egui::Color32, b: egui::Color32, t: f32) -> egui::Color32 {
    let t = t.clamp(0.0, 1.0);
    let lerp = |x: u8, y: u8| -> u8 {
        ((x as f32) * (1.0 - t) + (y as f32) * t)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    egui::Color32::from_rgb(lerp(a.r(), b.r()), lerp(a.g(), b.g()), lerp(a.b(), b.b()))
}

// ---------- Compose bar -------------------------------------------------------

fn compose_bar(
    ui: &mut egui::Ui,
    state: &mut DebugUiState,
    chat: &ChatState,
    gateway: Option<&GatewayClientHandle>,
    airi_events: Option<&mut AiriHaEventQueue>,
    echo: &mut MessageWriter<LocalUserEchoMessage>,
) {
    attachment_strip(ui, state);

    ui.horizontal(|ui| {
        let text_area = egui::TextEdit::multiline(&mut state.chat.input)
            .hint_text("type a message · Ctrl+Enter to send · drop images to attach")
            .desired_rows(3)
            .lock_focus(true)
            .desired_width(ui.available_width() - 110.0);
        let response = ui.add(text_area);

        ui.vertical(|ui| {
            if ui
                .add_enabled(gateway.is_some(), egui::Button::new("📎 Attach"))
                .on_disabled_hover_text("gateway unavailable")
                .clicked()
            {
                open_attach_dialog(&mut state.chat.pending);
            }

            let can_send = gateway.is_some()
                && (!state.chat.input.trim().is_empty() || !state.chat.pending.is_empty());
            if ui
                .add_enabled(can_send, egui::Button::new("Send"))
                .clicked()
                || (response.has_focus()
                    && ui.input(|i| i.modifiers.ctrl || i.modifiers.command)
                    && ui.input(|i| i.key_pressed(egui::Key::Enter))
                    && can_send)
            {
                send_current(state, chat, gateway, airi_events, echo);
            }
        });
    });
}

fn attachment_strip(ui: &mut egui::Ui, state: &mut DebugUiState) {
    if state.chat.pending.is_empty() {
        return;
    }
    ui.horizontal_wrapped(|ui| {
        let mut remove: Option<usize> = None;
        for (idx, att) in state.chat.pending.iter().enumerate() {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(format!("🖼 {}", att.name));
                    if ui.button("✕").on_hover_text("remove").clicked() {
                        remove = Some(idx);
                    }
                });
            });
        }
        if let Some(idx) = remove {
            state.chat.pending.remove(idx);
        }
    });
}

fn open_attach_dialog(pending: &mut Vec<PendingAttachment>) {
    let files = rfd::FileDialog::new()
        .add_filter("Images", &["png", "jpg", "jpeg", "gif", "webp", "bmp"])
        .pick_files();
    let Some(files) = files else { return };
    for path in files {
        match GatewayClient::attach_file(&path) {
            Ok(image) => {
                let name = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("attachment")
                    .to_string();
                pending.push(PendingAttachment { name, image });
            }
            Err(e) => {
                warn!("attach_file({}) failed: {e}", path.display());
            }
        }
    }
}

fn send_current(
    state: &mut DebugUiState,
    chat: &ChatState,
    gateway: Option<&GatewayClientHandle>,
    airi_events: Option<&mut AiriHaEventQueue>,
    echo: &mut MessageWriter<LocalUserEchoMessage>,
) {
    let Some(g) = gateway else { return };
    let text = state.chat.input.trim().to_string();
    let images: Vec<ImageData> = state.chat.pending.drain(..).map(|att| att.image).collect();
    let attachment_count = images.len();
    let thread = chat.active_thread.clone();
    let outbound_text = if let Some(queue) = airi_events {
        if let Some(ctx) = queue.take_context_block() {
            format!("{ctx}\n\nUser: {text}")
        } else {
            text.clone()
        }
    } else {
        text.clone()
    };
    if images.is_empty() {
        g.send_text(outbound_text, thread);
    } else {
        g.send_with_images(outbound_text, thread, images);
    }
    // Local echo — make the user's own message appear in the transcript
    // immediately instead of waiting for the gateway's roundtrip. The
    // SSE `Response` path does NOT re-echo user messages (only assistant
    // ones), and on error it never fires at all, so without this the
    // bubble disappears until the user clicks the thread and forces a
    // history reload.
    echo.write(LocalUserEchoMessage {
        text,
        attachments: attachment_count,
    });
    state.chat.input.clear();
}

// ---------- Drag and drop -----------------------------------------------------

fn ingest_dropped_files(ctx: &egui::Context, state: &mut DebugUiState) {
    let dropped = ctx.input(|i| i.raw.dropped_files.clone());
    if dropped.is_empty() {
        return;
    }
    for file in dropped {
        let name = if !file.name.is_empty() {
            file.name.clone()
        } else if let Some(path) = &file.path {
            path.file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("dropped")
                .to_string()
        } else {
            "dropped".to_string()
        };

        let result = if let Some(path) = &file.path {
            GatewayClient::attach_file(path)
        } else if let Some(bytes) = &file.bytes {
            attach_inline_bytes(&name, bytes)
        } else {
            warn!("dropped file '{name}' has no path or bytes; skipping");
            continue;
        };

        match result {
            Ok(image) => state.chat.pending.push(PendingAttachment { name, image }),
            Err(e) => warn!("drag-and-drop attach failed: {e}"),
        }
    }
}

fn attach_inline_bytes(
    name: &str,
    bytes: &[u8],
) -> Result<ImageData, jarvis_avatar::ironclaw::client::GatewayError> {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as B64;

    let mime = mime_from_name(name).ok_or_else(|| {
        jarvis_avatar::ironclaw::client::GatewayError::MissingMime(name.to_string())
    })?;
    Ok(ImageData {
        media_type: mime.into(),
        data: B64.encode(bytes),
    })
}

fn mime_from_name(name: &str) -> Option<&'static str> {
    let ext = name.rsplit('.').next()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        _ => return None,
    })
}

fn draw_drop_overlay(ctx: &egui::Context) {
    egui::Area::new(egui::Id::new("chat_drop_overlay"))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style())
                .fill(egui::Color32::from_rgba_unmultiplied(30, 30, 30, 220))
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new("⤓  Drop image to attach")
                            .size(18.0)
                            .color(egui::Color32::WHITE),
                    );
                });
        });
}

// ---------- Helpers -----------------------------------------------------------

fn short_id(id: &str) -> String {
    if id.len() > 8 {
        id[..8].to_string()
    } else {
        id.to_string()
    }
}
