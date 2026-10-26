//! The Count's colours, from the docs site's theme (docs/src/styles/custom.css):
//! coral wings, a gold wrench and a purple outline. Help, the report, the
//! spinner and the TUI all paint from here.
//!
//! Each colour carries its own fallbacks: truecolor when the terminal says it
//! has it, the nearest xterm-256 entry when it has 256 colours, and an ANSI-16
//! code otherwise. Whether to colour at all is decided elsewhere (--color,
//! NO_COLOR, a terminal); this module only says which colour.
use ratatui::style::Color;
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Colour {
    pub rgb: (u8, u8, u8),
    /// The nearest xterm-256 index.
    pub indexed: u8,
    /// The ANSI-16 foreground code, 30–37 or 90–97.
    pub ansi: u8,
}

/// Coral, the brand accent: headings, the spinner, the TUI's title and tabs.
pub const ACCENT: Colour = Colour::new((232, 90, 79), 203, 91);
/// Lavender from the purple outline: column headers and quieter labels.
pub const ACCENT_DIM: Colour = Colour::new((167, 123, 187), 139, 35);
pub const SUCCESS: Colour = Colour::new((140, 207, 126), 114, 32);
/// The wrench's gold.
pub const WARNING: Colour = Colour::new((242, 181, 68), 215, 33);
/// A raspberry red, kept apart from the coral accent.
pub const ERROR: Colour = Colour::new((240, 80, 110), 204, 31);
/// The docs' grey with a purple cast.
pub const MUTED: Colour = Colour::new((140, 130, 150), 245, 90);
/// The TUI's cursor row, a background.
pub const SELECTION: Colour = Colour::new((50, 36, 60), 236, 90);

/// The bat on the help page, top to bottom: coral to gold, like the title on
/// the docs' landing page.
pub const GRADIENT: [Colour; 5] = [
    ACCENT,
    Colour::new((235, 113, 74), 209, 91),
    Colour::new((237, 136, 69), 209, 33),
    Colour::new((240, 158, 68), 215, 33),
    WARNING,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Depth {
    Ansi,
    Indexed,
    True,
}

impl Depth {
    /// What the terminal says it can draw: COLORTERM for truecolor, then
    /// TERM for 256 colours.
    pub fn from_env(colorterm: Option<&str>, term: Option<&str>) -> Self {
        if matches!(colorterm, Some("truecolor" | "24bit")) {
            Depth::True
        } else if term.is_some_and(|t| t.contains("256color") || t.contains("direct")) {
            Depth::Indexed
        } else {
            Depth::Ansi
        }
    }
}

/// This process's terminal depth, read once.
pub fn depth() -> Depth {
    static DEPTH: OnceLock<Depth> = OnceLock::new();
    *DEPTH.get_or_init(|| {
        let var = |name| std::env::var(name).ok();
        Depth::from_env(var("COLORTERM").as_deref(), var("TERM").as_deref())
    })
}

impl Colour {
    const fn new(rgb: (u8, u8, u8), indexed: u8, ansi: u8) -> Self {
        Self { rgb, indexed, ansi }
    }

    /// The SGR parameters that set this as the foreground.
    pub fn sgr(self, depth: Depth) -> String {
        let (r, g, b) = self.rgb;
        match depth {
            Depth::True => format!("38;2;{r};{g};{b}"),
            Depth::Indexed => format!("38;5;{}", self.indexed),
            Depth::Ansi => self.ansi.to_string(),
        }
    }

    /// The colour for ratatui at this terminal's depth.
    pub fn tui(self) -> Color {
        self.at(depth())
    }

    pub fn at(self, depth: Depth) -> Color {
        let (r, g, b) = self.rgb;
        match depth {
            Depth::True => Color::Rgb(r, g, b),
            Depth::Indexed => Color::Indexed(self.indexed),
            Depth::Ansi => match self.ansi {
                31 => Color::Red,
                32 => Color::Green,
                33 => Color::Yellow,
                35 => Color::Magenta,
                90 => Color::DarkGray,
                91 => Color::LightRed,
                _ => Color::Reset,
            },
        }
    }
}

/// The style usage-rs paints help headings with. Its palette has only the
/// sixteen ANSI names, so headings go out cyan and [help] repaints them.
pub const HELP_HEADING: &str = "cyan+bold";

/// A rendered help page in the brand colours. usage-rs writes cyan, bold for
/// headings and plain for inline code, and bright yellow (the bat's `logo_style`, one escape pair per art line);
/// this swaps them for the accent, the dim accent and the gradient at the
/// terminal's depth.
///
/// usage-rs ends each art line with a full reset, which would also end a
/// style the page left open on that line, such as the dim text beside the
/// bat. So the art is painted from a clean state and the page's style is put
/// back after it. A plain page has no escapes and comes back unchanged.
pub fn help(page: &str, depth: Depth) -> String {
    let mut out = String::with_capacity(page.len() + 256);
    let mut active: Vec<String> = Vec::new();
    let mut art = 0;
    let mut in_art = false;
    let mut rest = page;
    while let Some(start) = rest.find("\x1b[") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find(|c: char| ('\x40'..='\x7e').contains(&c)) else {
            out.push_str(&rest[start..]);
            return out;
        };
        let (params, command) = (&after[..end], &after[end..=end]);
        rest = &after[end + 1..];
        if command != "m" {
            out.push_str(&format!("\x1b[{params}{command}"));
            continue;
        }
        let codes: Vec<&str> = params.split(';').filter(|c| !c.is_empty()).collect();
        if codes.contains(&"93") {
            let colour = GRADIENT[art.min(GRADIENT.len() - 1)];
            art += 1;
            in_art = true;
            out.push_str(&format!("\x1b[0;{}m", colour.sgr(depth)));
            continue;
        }
        if in_art {
            in_art = false;
            out.push_str("\x1b[0m");
            if !active.is_empty() {
                out.push_str(&format!("\x1b[{}m", active.join(";")));
            }
            continue;
        }
        if codes.is_empty() {
            active.clear();
        }
        let teal = if codes.contains(&"1") {
            ACCENT
        } else {
            ACCENT_DIM
        };
        let mut mapped = Vec::new();
        for code in codes {
            let code = if code == "36" {
                teal.sgr(depth)
            } else {
                code.to_owned()
            };
            match code.as_str() {
                "0" => active.clear(),
                "39" => active.retain(|c| !foreground(c)),
                "22" => active.retain(|c| c != "1" && c != "2"),
                _ => {
                    if foreground(&code) {
                        active.retain(|c| !foreground(c));
                    }
                    if !active.contains(&code) {
                        active.push(code.clone());
                    }
                }
            }
            mapped.push(code);
        }
        out.push_str(&format!("\x1b[{}m", mapped.join(";")));
    }
    out.push_str(rest);
    out
}

fn foreground(code: &str) -> bool {
    code.starts_with("38;")
        || code
            .parse::<u8>()
            .is_ok_and(|n| (30..=37).contains(&n) || (90..=97).contains(&n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn depth_follows_colorterm_then_term() {
        assert_eq!(
            Depth::from_env(Some("truecolor"), Some("xterm")),
            Depth::True
        );
        assert_eq!(Depth::from_env(Some("24bit"), None), Depth::True);
        assert_eq!(
            Depth::from_env(None, Some("xterm-256color")),
            Depth::Indexed
        );
        assert_eq!(Depth::from_env(None, Some("xterm")), Depth::Ansi);
        assert_eq!(Depth::from_env(None, None), Depth::Ansi);
    }

    #[test]
    fn colours_fall_back_by_depth() {
        assert_eq!(ACCENT.sgr(Depth::True), "38;2;232;90;79");
        assert_eq!(ACCENT.sgr(Depth::Indexed), "38;5;203");
        assert_eq!(ACCENT.sgr(Depth::Ansi), "91");
        assert_eq!(ACCENT.at(Depth::True), Color::Rgb(232, 90, 79));
        assert_eq!(ACCENT.at(Depth::Indexed), Color::Indexed(203));
        assert_eq!(ACCENT.at(Depth::Ansi), Color::LightRed);
    }

    #[test]
    fn help_repaints_headings_and_the_bat_and_keeps_the_page_style() {
        let page = "\x1b[2mabout \x1b[36mcode\x1b[39m\x1b[2m \x1b[93m⣿⣿\x1b[0m\nmore\x1b[0m\n\x1b[1;36mUsage:\x1b[0m x\n  \x1b[93m⠈⠁\x1b[0m\n";
        assert_eq!(
            help(page, Depth::True),
            "\x1b[2mabout \x1b[38;2;167;123;187mcode\x1b[39m\x1b[2m \x1b[0;38;2;232;90;79m⣿⣿\x1b[0m\x1b[2m\nmore\x1b[0m\n\
             \x1b[1;38;2;232;90;79mUsage:\x1b[0m x\n  \x1b[0;38;2;235;113;74m⠈⠁\x1b[0m\n"
        );
        assert_eq!(help("plain\n", Depth::Indexed), "plain\n");
    }
}
