pub use egui_phosphor::regular as p;

use bevy_egui::egui::{Color32, FontDefinitions, FontFamily, RichText};

pub fn install_fonts(fonts: &mut FontDefinitions) {
    egui_phosphor::add_to_fonts(fonts, egui_phosphor::Variant::Regular);

    for family in [
        FontFamily::Name("Regular".into()),
        FontFamily::Name("Bold".into()),
    ] {
        if let Some(keys) = fonts.families.get_mut(&family) {
            if !keys.iter().any(|k| k == "phosphor") {
                keys.insert(1.min(keys.len()), "phosphor".into());
            }
        }
    }
}

pub fn icon(glyph: &str) -> RichText {
    RichText::new(glyph).family(FontFamily::Proportional)
}

pub fn icon_sized(glyph: &str, size: f32) -> RichText {
    RichText::new(glyph).size(size).family(FontFamily::Proportional)
}

pub fn icon_colored(glyph: &str, color: Color32) -> RichText {
    icon(glyph).color(color)
}

pub fn menu_label(name: &str) -> String {
    format!("{name} {}", p::CARET_DOWN)
}

pub fn menu_item(icon_glyph: &str, label: &str) -> String {
    format!("{icon_glyph} {label}")
}

pub const MENU_SUFFIX: &str = p::CARET_DOWN;
pub const CHEV_OPEN: &str = p::CARET_DOWN;
pub const CHEV_CLOSED: &str = p::CARET_RIGHT;

pub const STOP: &str = p::STOP;
pub const PLAY: &str = p::PLAY;
pub const PAUSE: &str = p::PAUSE;
pub const REWIND: &str = p::SKIP_BACK;

pub const UP: &str = p::ARROW_UP;
pub const DOWN: &str = p::ARROW_DOWN;
pub const ARROW_RIGHT: &str = p::ARROW_RIGHT;
pub const REFRESH: &str = p::ARROW_CLOCKWISE;
pub const REVERSE: &str = p::ARROW_COUNTER_CLOCKWISE;
pub const MIRROR: &str = p::FLIP_HORIZONTAL;
pub const INFINITY: &str = p::INFINITY;
pub const SUBTREE: &str = p::ARROW_ELBOW_DOWN_RIGHT;
pub const HOME: &str = p::HOUSE;

pub const STATUS_ON: &str = p::CHECK_CIRCLE;
pub const STATUS_READY: &str = p::CHECK;
pub const STATUS_WARN: &str = p::WARNING;
pub const STATUS_ERR: &str = p::X;
pub const STATUS_OFF: &str = p::MINUS_CIRCLE;
pub const STATUS_DISABLED: &str = p::PROHIBIT;
pub const STATUS_IDLE: &str = p::CIRCLE;
pub const STATUS_WAIT: &str = p::HOURGLASS_SIMPLE;
pub const STATUS_QUEUED: &str = p::CLOCK;
pub const STATUS_DOT: &str = p::DOT;

pub const INFO: &str = p::INFO;
pub const CRITICAL: &str = p::SKULL;

pub const FOCUS: &str = p::CROSSHAIR;
pub const OPEN: &str = p::ARROWS_OUT_SIMPLE;
pub const CLOSE: &str = p::X;
pub const FLOATING: &str = p::APP_WINDOW;
pub const HIDDEN: &str = p::EYE_SLASH;
pub const DOCK_LEFT: &str = p::CARET_LEFT;
pub const DOCK_RIGHT: &str = p::CARET_RIGHT;
pub const DOCK_BOTTOM: &str = p::CARET_DOWN;

pub const BETA: &str = p::TEST_TUBE;
pub const BETA_TAG: &str = BETA;

pub const LOCK: &str = p::LOCK;
pub const POWER: &str = p::POWER;
pub const TERMINAL: &str = p::TERMINAL;
pub const DESKTOP: &str = p::DESKTOP;
pub const SIGN_OUT: &str = p::SIGN_OUT;

pub const FOLDER: &str = p::FOLDER;
pub const FOLDER_OPEN: &str = p::FOLDER_OPEN;
pub const FILE: &str = p::FILE;
pub const FILE_TEXT: &str = p::FILE_TEXT;
pub const PACKAGE: &str = p::PACKAGE;

pub const DOWNLOAD: &str = p::DOWNLOAD;
pub const UPLOAD: &str = p::UPLOAD;
pub const ATTACH: &str = p::PAPERCLIP;
pub const TRASH: &str = p::TRASH;
pub const SAVE: &str = p::FLOPPY_DISK;
pub const EYE: &str = p::EYE;
pub const SEARCH: &str = p::MAGNIFYING_GLASS;
pub const GEAR: &str = p::GEAR;
pub const PLUS: &str = p::PLUS;
pub const MINUS: &str = p::MINUS;
pub const EDIT: &str = p::PENCIL;
pub const COPY: &str = p::COPY;
pub const CLIPBOARD: &str = p::CLIPBOARD;
pub const LIST: &str = p::LIST;
pub const SCROLL: &str = p::SCROLL;
pub const CHART: &str = p::CHART_BAR;
pub const WRENCH: &str = p::WRENCH;
pub const LIGHTBULB: &str = p::LIGHTBULB;
pub const ROBOT: &str = p::ROBOT;
pub const STAR: &str = p::STAR;
pub const CHAT: &str = p::CHAT;
pub const BELL: &str = p::BELL;
pub const GAME: &str = p::GAME_CONTROLLER;
pub const FLASK: &str = p::FLASK;
pub const MONITOR: &str = p::MONITOR;
pub const BOOK: &str = p::BOOK;
pub const MUSIC: &str = p::MUSIC_NOTES;
pub const IMAGE: &str = p::IMAGE;
pub const CAMERA: &str = p::CAMERA;
pub const MAGIC: &str = p::MAGIC_WAND;
pub const VIDEO: &str = p::VIDEO;
pub const HARD_DRIVE: &str = p::HARD_DRIVE;
pub const DIAGNOSTICS: &str = p::MICROSCOPE;
pub const GRID: &str = p::SQUARES_FOUR;
pub const CARET_LEFT: &str = p::CARET_LEFT;
pub const CARET_RIGHT: &str = p::CARET_RIGHT;