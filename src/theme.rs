use ratatui::style::Color;

/// Raw palette: 11 named colors as RGB. All UI code goes through the
/// semantic accessors on [`Theme`], never raw hex.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub bg: (u8, u8, u8),
    pub bg_dark: (u8, u8, u8),
    pub fg: (u8, u8, u8),
    pub muted: (u8, u8, u8),
    pub green: (u8, u8, u8),
    pub cyan: (u8, u8, u8),
    pub purple: (u8, u8, u8),
    pub pink: (u8, u8, u8),
    pub red: (u8, u8, u8),
    pub orange: (u8, u8, u8),
    pub yellow: (u8, u8, u8),
}

pub const ELDRITCH: Palette = Palette {
    bg: (0x21, 0x23, 0x37),
    bg_dark: (0x17, 0x19, 0x28),
    fg: (0xeb, 0xfa, 0xfa),
    muted: (0x70, 0x81, 0xd0),
    green: (0x37, 0xf4, 0x99),
    cyan: (0x04, 0xd1, 0xf9),
    purple: (0xa4, 0x8c, 0xf2),
    pink: (0xf2, 0x65, 0xb5),
    red: (0xf1, 0x6c, 0x75),
    orange: (0xf7, 0xc6, 0x7f),
    yellow: (0xf1, 0xfc, 0x79),
};

pub const TOKYONIGHT: Palette = Palette {
    bg: (0x1a, 0x1b, 0x26),
    bg_dark: (0x16, 0x16, 0x1e),
    fg: (0xc0, 0xca, 0xf5),
    muted: (0x56, 0x5f, 0x89),
    green: (0x9e, 0xce, 0x6a),
    cyan: (0x7d, 0xcf, 0xff),
    purple: (0xbb, 0x9a, 0xf7),
    pink: (0xff, 0x75, 0xa8),
    red: (0xf7, 0x76, 0x8e),
    orange: (0xff, 0x9e, 0x64),
    yellow: (0xe0, 0xaf, 0x68),
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
}

fn rgb(c: (u8, u8, u8)) -> Color {
    Color::Rgb(c.0, c.1, c.2)
}

// The ratatui color accessors come alive with the TUI (M3); non-TUI output
// reaches the palette directly through `Theme::owo`.
#[allow(dead_code)]
impl Theme {
    pub fn by_name(name: &str, icons: IconSet) -> Option<Self> {
        let palette = match name {
            "eldritch" => ELDRITCH,
            "tokyonight" => TOKYONIGHT,
            _ => return None,
        };
        Some(Self { palette, icons })
    }

    // Semantic colors (ratatui).
    pub fn bg(&self) -> Color {
        rgb(self.palette.bg)
    }
    pub fn bg_dark(&self) -> Color {
        rgb(self.palette.bg_dark)
    }
    pub fn fg(&self) -> Color {
        rgb(self.palette.fg)
    }
    pub fn muted(&self) -> Color {
        rgb(self.palette.muted)
    }
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
        rgb(self.palette.pink)
    }
    pub fn attention(&self) -> Color {
        rgb(self.palette.yellow)
    }

    // Semantic colors (owo-colors, for non-TUI styled output).
    pub fn owo(&self, c: (u8, u8, u8)) -> owo_colors::Rgb {
        owo_colors::Rgb(c.0, c.1, c.2)
    }

    // Icons.
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
        }
    }
}
