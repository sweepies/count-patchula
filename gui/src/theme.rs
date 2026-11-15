//! The Count's colours for the window: the docs site's night sky, coral wings
//! and gold wrench, as a rune-kit theme, plus the colour of each update type.
use patchula_words::{Severity, Update as UpdateKind};
use rune::{Color, color::hex};
use rune_kit::{Kit, PaletteFocus, PaletteSeed};

fn rgb(code: &str) -> Color {
    hex(code).unwrap_or(Color::WHITE)
}

/// The brand colours that are not kit tokens.
#[derive(Clone, Copy)]
pub struct Brand {
    pub coral: Color,
    pub gold: Color,
    pub purple: Color,
}

pub fn brand(dark: bool) -> Brand {
    if dark {
        Brand {
            coral: rgb("#e85a4f"),
            gold: rgb("#f2b544"),
            purple: rgb("#a77bbb"),
        }
    } else {
        Brand {
            coral: rgb("#b9382f"),
            gold: rgb("#a8700a"),
            purple: rgb("#6b4a7a"),
        }
    }
}

/// The kit every view draws with: surfaces, text and borders derived from the
/// docs' background, ink and accent, so both appearances keep the purple cast.
pub fn kit(dark: bool) -> Kit {
    let seed = if dark {
        PaletteSeed {
            bg: rgb("#19111c"),
            fg: rgb("#f1ecf3"),
            accent: rgb("#e85a4f"),
        }
    } else {
        PaletteSeed {
            bg: rgb("#f9f7fa"),
            fg: rgb("#19111c"),
            accent: rgb("#b9382f"),
        }
    };
    let mut kit = Kit::from_palette_focus(seed, PaletteFocus::Accent);
    kit.radius.md = 8.0;
    kit.radius.lg = 12.0;
    kit
}

/// The colour of an update type's mark. Each appearance has its own steps of
/// the CLI's hues, checked for colour-vision separation between neighbours in
/// the Count's ranked order; digest takes blue so it is not pin's lavender.
pub fn kind(kind: UpdateKind, dark: bool) -> Color {
    rgb(match (kind, dark) {
        (UpdateKind::Major, true) => "#e04a69",
        (UpdateKind::Minor, true) => "#be8a1c",
        (UpdateKind::Patch, true) => "#34966a",
        (UpdateKind::Pin, true) => "#aa7fc2",
        (UpdateKind::Digest, true) => "#3a6fc0",
        (UpdateKind::Major, false) => "#c5304f",
        (UpdateKind::Minor, false) => "#b8800c",
        (UpdateKind::Patch, false) => "#2a7a50",
        (UpdateKind::Pin, false) => "#a577c2",
        (UpdateKind::Digest, false) => "#1f5fa8",
    })
}

/// An advisory severity's status colour: the kit's danger for critical and
/// high, warning for moderate, and plain muted ink below that.
pub fn severity(severity: Option<Severity>, k: &Kit) -> Color {
    match severity {
        Some(Severity::Critical | Severity::High) => k.c.danger.icon,
        Some(Severity::Moderate) => k.c.warning.icon,
        Some(Severity::Low) | None => k.c.icon_muted,
    }
}
