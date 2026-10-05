pub type Rgb = [u8; 3];

pub struct Theme {
    pub name: &'static str,
    /// Behind the terminal panel.
    pub canvas: Rgb,
    pub background: Rgb,
    pub foreground: Rgb,
    pub cursor: Rgb,
    /// The line between stacked panels.
    pub divider: Rgb,
    pub ansi: [Rgb; 16],
}

const fn hex(v: u32) -> Rgb {
    [(v >> 16) as u8, (v >> 8) as u8, v as u8]
}

pub const THEMES: &[Theme] = &[
    Theme {
        name: "dark",
        canvas: hex(0x0b0c10),
        background: hex(0x17191f),
        foreground: hex(0xd9dee8),
        cursor: hex(0xe8e8e8),
        divider: hex(0x2b2f3a),
        ansi: [
            hex(0x2b2f3a),
            hex(0xff6b7f),
            hex(0x9ece6a),
            hex(0xe0af68),
            hex(0x7aa2f7),
            hex(0xc49bf7),
            hex(0x7dcfff),
            hex(0xc0caf5),
            hex(0x5c6480),
            hex(0xff8a9b),
            hex(0xb5e88a),
            hex(0xffc777),
            hex(0x94b4ff),
            hex(0xd7b4ff),
            hex(0xa4daff),
            hex(0xeef1ff),
        ],
    },
    Theme {
        name: "light",
        canvas: hex(0xdfe1e6),
        background: hex(0xfafafa),
        foreground: hex(0x383a42),
        cursor: hex(0x526fff),
        divider: hex(0xc4c7ce),
        ansi: [
            hex(0x383a42),
            hex(0xd73a49),
            hex(0x22863a),
            hex(0xb08800),
            hex(0x0366d6),
            hex(0x8f44c2),
            hex(0x0e8a9e),
            hex(0xa0a1a7),
            hex(0x696c77),
            hex(0xe45649),
            hex(0x50a14f),
            hex(0xc18401),
            hex(0x4078f2),
            hex(0xa626a4),
            hex(0x0184bc),
            hex(0xffffff),
        ],
    },
];

pub fn by_name(name: &str) -> Option<&'static Theme> {
    THEMES.iter().find(|t| t.name == name)
}

impl Theme {
    pub fn color(&self, c: vt100::Color, default: Rgb) -> Rgb {
        match c {
            vt100::Color::Default => default,
            vt100::Color::Rgb(r, g, b) => [r, g, b],
            vt100::Color::Idx(i) => self.indexed(i),
        }
    }

    fn indexed(&self, i: u8) -> Rgb {
        match i {
            0..=15 => self.ansi[i as usize],
            16..=231 => {
                const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
                let n = i - 16;
                [
                    LEVELS[(n / 36) as usize],
                    LEVELS[(n / 6 % 6) as usize],
                    LEVELS[(n % 6) as usize],
                ]
            }
            232..=255 => {
                let v = 8 + 10 * (i - 232);
                [v, v, v]
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vt100::Color;

    #[test]
    fn xterm_256_palette() {
        let t = by_name("dark").unwrap();
        assert_eq!(t.color(Color::Idx(1), [0; 3]), t.ansi[1]);
        assert_eq!(t.color(Color::Idx(16), [9; 3]), [0, 0, 0]);
        assert_eq!(t.color(Color::Idx(196), [0; 3]), [255, 0, 0]);
        assert_eq!(t.color(Color::Idx(231), [0; 3]), [255, 255, 255]);
        assert_eq!(t.color(Color::Idx(232), [0; 3]), [8, 8, 8]);
        assert_eq!(t.color(Color::Idx(255), [0; 3]), [238, 238, 238]);
        assert_eq!(t.color(Color::Rgb(1, 2, 3), [0; 3]), [1, 2, 3]);
        assert_eq!(t.color(Color::Default, [7; 3]), [7, 7, 7]);
    }
}
