//! Coarse “where is the chat pipeline?” indicator for the menu bar and ops.
//!
//! This is **best-effort** (no strict ordering guarantees across plugins). It
//! reflects the last subsystem that updated the stage during the current turn.

use std::time::Instant;

use bevy::prelude::*;

/// High-level chat / voice / face pipeline stage.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ChatPipelineStage {
    #[default]
    Idle,
    /// SSE `stream_chunk` tokens arriving.
    AiStreaming,
    /// Model reasoning / chain-of-thought line.
    AiThinking,
    /// Final assistant `Response` received; Kokoro request queued (or skipped).
    KokoroQueued,
    /// Kokoro HTTP request in flight (TTS worker).
    KokoroSynthesizing,
    /// PCM/WAV handed to Bevy audio (clip spawned).
    KokoroPlaying,
    /// `ExpressionsPlugin` applied ACT → VRM weights / clip.
    ApplyingActToVrm,
    /// Gateway tool lifecycle.
    ToolRunning,
    /// A2F gRPC completed; VRM expression clip is playing (lip-sync from chat TTS).
    A2fLipSync,
}

impl ChatPipelineStage {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Idle => "Idle",
            Self::AiStreaming => "AI · streaming",
            Self::AiThinking => "AI · thinking",
            Self::KokoroQueued => "TTS · queued",
            Self::KokoroSynthesizing => "TTS · Kokoro",
            Self::KokoroPlaying => "TTS · playing",
            Self::ApplyingActToVrm => "Face · ACT→VRM",
            Self::ToolRunning => "Tool",
            Self::A2fLipSync => "Face · A2F lip-sync",
        }
    }
}

#[derive(Resource, Clone, Debug)]
pub struct ChatPipelineStatus {
    pub stage: ChatPipelineStage,
    pub detail: String,
    pub updated: Instant,
}

impl Default for ChatPipelineStatus {
    fn default() -> Self {
        Self {
            stage: ChatPipelineStage::Idle,
            detail: String::new(),
            updated: Instant::now(),
        }
    }
}

impl ChatPipelineStatus {
    pub fn set(&mut self, stage: ChatPipelineStage, detail: impl Into<String>) {
        self.stage = stage;
        self.detail = detail.into();
        self.updated = Instant::now();
    }

    /// One line for the menu bar: `Stage — detail`.
    pub fn menu_line(&self) -> String {
        let d = self.detail.trim();
        if d.is_empty() {
            self.stage.label().to_string()
        } else {
            format!("{} — {}", self.stage.label(), d)
        }
    }
}
