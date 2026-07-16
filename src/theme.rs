//! base46-faithful theming. The eldritch palette is copied VERBATIM from the
//! user's NvChad install (base46/themes/eldritch.lua); tokyonight carries the
//! canonical base46 values. All UI code goes through the semantic accessors;
//! never raw hex outside this file.

use ratatui::style::Color;

/// Full base_30 ladder, straight from base46.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub white: (u8, u8, u8),
    pub darker_black: (u8, u8, u8),
    pub black: (u8, u8, u8),
    pub black2: (u8, u8, u8),
    pub one_bg: (u8, u8, u8),
    pub one_bg2: (u8, u8, u8),
    pub one_bg3: (u8, u8, u8),
    pub grey: (u8, u8, u8),
    pub grey_fg: (u8, u8, u8),
    pub light_grey: (u8, u8, u8),
    pub line: (u8, u8, u8),
    pub statusline_bg: (u8, u8, u8),
    pub red: (u8, u8, u8),
    pub baby_pink: (u8, u8, u8),
    pub pink: (u8, u8, u8),
    pub green: (u8, u8, u8),
    pub vibrant_green: (u8, u8, u8),
    pub nord_blue: (u8, u8, u8),
    pub blue: (u8, u8, u8),
    pub yellow: (u8, u8, u8),
    pub sun: (u8, u8, u8),
    pub purple: (u8, u8, u8),
    pub dark_purple: (u8, u8, u8),
    pub teal: (u8, u8, u8),
    pub orange: (u8, u8, u8),
    pub cyan: (u8, u8, u8),
    pub pmenu_bg: (u8, u8, u8),
    pub folder_bg: (u8, u8, u8),
}

pub const ELDRITCH: Palette = Palette {
    white: (0xEB, 0xFA, 0xFA),
    darker_black: (0x13, 0x14, 0x21),
    black: (0x17, 0x19, 0x28),
    black2: (0x20, 0x23, 0x38),
    one_bg: (0x29, 0x2D, 0x48),
    one_bg2: (0x32, 0x37, 0x58),
    one_bg3: (0x32, 0x37, 0x58),
    grey: (0x44, 0x4B, 0x78),
    grey_fg: (0x4D, 0x55, 0x88),
    light_grey: (0x67, 0x70, 0xAA),
    line: (0x3B, 0x42, 0x61),
    statusline_bg: (0x1E, 0x21, 0x34),
    red: (0xF1, 0x6C, 0x75),
    baby_pink: (0xF2, 0x65, 0xB5),
    pink: (0xBF, 0x4F, 0x8E),
    green: (0x37, 0xF4, 0x99),
    vibrant_green: (0x00, 0xFA, 0x82),
    nord_blue: (0x70, 0x81, 0xD0),
    blue: (0x04, 0xD1, 0xF9),
    yellow: (0xF1, 0xFC, 0x79),
    sun: (0xE9, 0xF9, 0x41),
    purple: (0xA4, 0x8C, 0xF2),
    dark_purple: (0x58, 0x66, 0xA2),
    teal: (0x33, 0xC5, 0x7F),
    orange: (0xF7, 0xC6, 0x7F),
    cyan: (0x04, 0xD1, 0xF9),
    pmenu_bg: (0x37, 0xF4, 0x99),
    folder_bg: (0x66, 0xE4, 0xFD),
};

pub const TOKYONIGHT: Palette = Palette {
    white: (0xC0, 0xCA, 0xF5),
    darker_black: (0x16, 0x16, 0x1E),
    black: (0x1A, 0x1B, 0x26),
    black2: (0x1F, 0x23, 0x36),
    one_bg: (0x24, 0x28, 0x3B),
    one_bg2: (0x41, 0x48, 0x68),
    one_bg3: (0x35, 0x3B, 0x45),
    grey: (0x40, 0x48, 0x6A),
    grey_fg: (0x56, 0x5F, 0x89),
    light_grey: (0x54, 0x5C, 0x7E),
    line: (0x32, 0x33, 0x3E),
    statusline_bg: (0x1D, 0x1E, 0x29),
    red: (0xF7, 0x76, 0x8E),
    baby_pink: (0xDE, 0x8C, 0x92),
    pink: (0xFF, 0x75, 0xA8),
    green: (0x9E, 0xCE, 0x6A),
    vibrant_green: (0x73, 0xDA, 0xCA),
    nord_blue: (0x80, 0xA8, 0xFD),
    blue: (0x7A, 0xA2, 0xF7),
    yellow: (0xE0, 0xAF, 0x68),
    sun: (0xEB, 0xCB, 0x8B),
    purple: (0xBB, 0x9A, 0xF7),
    dark_purple: (0x9D, 0x7C, 0xD8),
    teal: (0x1A, 0xBC, 0x9C),
    orange: (0xFF, 0x9E, 0x64),
    cyan: (0x7D, 0xCF, 0xFF),
    pmenu_bg: (0x7A, 0xA2, 0xF7),
    folder_bg: (0x7A, 0xA2, 0xF7),
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconSet {
    Nerd,
    Ascii,
}

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub palette: Palette,
    pub icons: IconSet,
    /// chadrc parity: terminal background shows through the main pane.
    pub transparency: bool,
}

fn rgb(c: (u8, u8, u8)) -> Color {
    Color::Rgb(c.0, c.1, c.2)
}

impl Theme {
    pub fn by_name(name: &str, icons: IconSet) -> Option<Self> {
        let palette = match name {
            "eldritch" => ELDRITCH,
            "tokyonight" => TOKYONIGHT,
            _ => return None,
        };
        Some(Self {
            palette,
            icons,
            transparency: true,
        })
    }

    // ---- background ladder -------------------------------------------------
    /// Main pane: terminal shows through under transparency (chadrc parity).
    pub fn bg(&self) -> Color {
        if self.transparency {
            Color::Reset
        } else {
            rgb(self.palette.black)
        }
    }
    pub fn bg_sidebar(&self) -> Color {
        rgb(self.palette.darker_black)
    }
    pub fn bg_statusline(&self) -> Color {
        rgb(self.palette.statusline_bg)
    }
    /// Floats stay opaque even in transparency mode (NormalFloat).
    pub fn bg_float(&self) -> Color {
        rgb(self.palette.black)
    }
    /// PmenuSel: purple selection, dark text.
    pub fn bg_selection(&self) -> Color {
        rgb(self.palette.purple)
    }
    /// CursorLine-ish row striping / soft emphasis.
    pub fn bg_row(&self) -> Color {
        rgb(self.palette.one_bg)
    }
    pub fn bg_row2(&self) -> Color {
        rgb(self.palette.one_bg2)
    }
    // Retained for non-TUI/status uses.
    pub fn bg_dark(&self) -> Color {
        rgb(self.palette.darker_black)
    }

    // ---- foregrounds ---------------------------------------------------
    pub fn fg(&self) -> Color {
        rgb(self.palette.white)
    }
    pub fn muted(&self) -> Color {
        rgb(self.palette.light_grey)
    }
    pub fn comment(&self) -> Color {
        rgb(self.palette.dark_purple)
    }
    pub fn grey_fg(&self) -> Color {
        rgb(self.palette.grey_fg)
    }
    pub fn divider(&self) -> Color {
        rgb(self.palette.line)
    }
    /// Dark text on colored powerline segments / pills.
    pub fn on_accent(&self) -> Color {
        rgb(self.palette.black)
    }

    // ---- accents (base46 treesitter semantics) --------------------------
    pub fn accent(&self) -> Color {
        rgb(self.palette.purple)
    }
    pub fn ok(&self) -> Color {
        rgb(self.palette.green)
    }
    pub fn info(&self) -> Color {
        rgb(self.palette.cyan)
    }
    pub fn warn(&self) -> Color {
        rgb(self.palette.orange)
    }
    pub fn error(&self) -> Color {
        rgb(self.palette.red)
    }
    pub fn highlight(&self) -> Color {
        rgb(self.palette.baby_pink)
    }
    pub fn attention(&self) -> Color {
        rgb(self.palette.yellow)
    }

    // Semantic colors (owo-colors, for non-TUI styled output).
    pub fn owo(&self, c: (u8, u8, u8)) -> owo_colors::Rgb {
        owo_colors::Rgb(c.0, c.1, c.2)
    }

    // ---- glyphs ----------------------------------------------------------
    // Statusline separators: the user's live NvChad style ("default"):
    // rounded left cap U+E0B6, slanted right separator U+E0BC.
    pub fn sep_left(&self) -> &'static str {
        self.pick("\u{e0b6}", "")
    }
    pub fn sep_right(&self) -> &'static str {
        self.pick("\u{e0bc}", "")
    }

    pub fn icon_pending(&self) -> &'static str {
        self.pick("○", "( )")
    }
    pub fn icon_running(&self) -> &'static str {
        self.pick("", ">>")
    }
    pub fn icon_done(&self) -> &'static str {
        self.pick("", "[x]")
    }
    pub fn icon_failed(&self) -> &'static str {
        self.pick("", "[!]")
    }
    pub fn icon_attention(&self) -> &'static str {
        self.pick("", "[?]")
    }
    pub fn icon_skipped(&self) -> &'static str {
        self.pick("", "(-)")
    }
    /// A Done stage whose inputs moved since it ran (guidance staleness).
    pub fn icon_stale(&self) -> &'static str {
        self.pick("\u{f021}", "[~]")
    }
    pub fn icon_branch(&self) -> &'static str {
        self.pick("", "br:")
    }
    pub fn icon_check(&self) -> &'static str {
        self.pick("", "chk")
    }
    pub fn icon_agent(&self) -> &'static str {
        self.pick("󰚩", "ai:")
    }
    pub fn icon_finding(&self) -> &'static str {
        self.pick("", "*")
    }
    pub fn icon_features(&self) -> &'static str {
        self.pick("󰮄", "~")
    }
    pub fn icon_pipeline(&self) -> &'static str {
        self.pick("", "#")
    }
    pub fn icon_bridge(&self) -> &'static str {
        self.pick("󱘖", "<>")
    }
    pub fn icon_nvim(&self) -> &'static str {
        self.pick("", "nv")
    }
    pub fn icon_prompt(&self) -> &'static str {
        self.pick("", ">")
    }
    pub fn icon_gutter(&self) -> &'static str {
        self.pick("▍", "|")
    }

    fn pick(&self, nerd: &'static str, ascii: &'static str) -> &'static str {
        match self.icons {
            IconSet::Nerd => nerd,
            IconSet::Ascii => ascii,
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            palette: ELDRITCH,
            icons: IconSet::Nerd,
            transparency: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ladders_are_ordered_dark_to_light() {
        for p in [ELDRITCH, TOKYONIGHT] {
            // The bg ladder must actually get lighter (sum of channels).
            let lum = |c: (u8, u8, u8)| c.0 as u16 + c.1 as u16 + c.2 as u16;
            assert!(lum(p.darker_black) < lum(p.black));
            assert!(lum(p.black) < lum(p.black2));
            assert!(lum(p.black2) < lum(p.one_bg));
            assert!(lum(p.one_bg) < lum(p.one_bg2));
            assert!(lum(p.grey) < lum(p.light_grey));
        }
    }

    #[test]
    fn transparency_switches_main_bg() {
        let mut t = Theme::default();
        assert_eq!(t.bg(), Color::Reset);
        t.transparency = false;
        assert!(matches!(t.bg(), Color::Rgb(..)));
        // Floats are always opaque.
        assert!(matches!(t.bg_float(), Color::Rgb(..)));
    }
}
