//! Monocode's color presets (workspaceThemes.ts): surface tints, ink and accent.
#[derive(Clone, Copy)]
pub(crate) struct Palette {
    pub surface: u32,
    pub content: u32,
    pub accent: u32,
}
pub(crate) const PALETTES: &[(&str, &str, Palette, Palette)] = &[
    (
        "default",
        "Default",
        Palette {
            surface: 0x0d0d0d,
            content: 0xebebeb,
            accent: 0x4da3f5,
        },
        Palette {
            surface: 0xf5f5f5,
            content: 0x2e2e2e,
            accent: 0x4da3f5,
        },
    ),
    (
        "harbor",
        "Harbor",
        Palette {
            surface: 0x0b1018,
            content: 0xdbe4f0,
            accent: 0x5b9cf2,
        },
        Palette {
            surface: 0xeef2f7,
            content: 0x1b2533,
            accent: 0x2f6fd0,
        },
    ),
    (
        "moss",
        "Moss",
        Palette {
            surface: 0x0d110e,
            content: 0xdde5dc,
            accent: 0x6fbf73,
        },
        Palette {
            surface: 0xf1f4ee,
            content: 0x1f2a20,
            accent: 0x3d8a45,
        },
    ),
    (
        "ember",
        "Ember",
        Palette {
            surface: 0x120e0c,
            content: 0xecdfd6,
            accent: 0xf08a4b,
        },
        Palette {
            surface: 0xf7f2ee,
            content: 0x2d211b,
            accent: 0xc75c1e,
        },
    ),
    (
        "orchid",
        "Orchid",
        Palette {
            surface: 0x110d16,
            content: 0xe6def0,
            accent: 0xb57cf0,
        },
        Palette {
            surface: 0xf4f0f8,
            content: 0x261e33,
            accent: 0x8048c8,
        },
    ),
    (
        "tidepool",
        "Tidepool",
        Palette {
            surface: 0x0a1213,
            content: 0xd8e8e8,
            accent: 0x3fc1b5,
        },
        Palette {
            surface: 0xedf5f5,
            content: 0x162a2b,
            accent: 0x138a80,
        },
    ),
    (
        "dune",
        "Dune",
        Palette {
            surface: 0x13110d,
            content: 0xebe3d2,
            accent: 0xd9b25c,
        },
        Palette {
            surface: 0xf6f1e7,
            content: 0x2e2819,
            accent: 0xa57d1e,
        },
    ),
    (
        "rosewood",
        "Rosewood",
        Palette {
            surface: 0x140e10,
            content: 0xefdfe3,
            accent: 0xe7798f,
        },
        Palette {
            surface: 0xf8f0f2,
            content: 0x2f1f24,
            accent: 0xc0405c,
        },
    ),
    (
        "glacier",
        "Glacier",
        Palette {
            surface: 0x0e1114,
            content: 0xe1e7ec,
            accent: 0x7fb4d9,
        },
        Palette {
            surface: 0xf2f5f8,
            content: 0x20262d,
            accent: 0x3a7aa8,
        },
    ),
    (
        "arcade",
        "Arcade",
        Palette {
            surface: 0x0f0b1a,
            content: 0xece6ff,
            accent: 0xff5fd2,
        },
        Palette {
            surface: 0xf5f2fc,
            content: 0x231a3a,
            accent: 0xc2189a,
        },
    ),
    (
        "ink",
        "Ink",
        Palette {
            surface: 0x000000,
            content: 0xf5f5f5,
            accent: 0x7aa7ff,
        },
        Palette {
            surface: 0xffffff,
            content: 0x000000,
            accent: 0x0050d8,
        },
    ),
];
pub(crate) fn palette(id: &str, light: bool) -> Palette {
    let row = PALETTES.iter().find(|r| r.0 == id).unwrap_or(&PALETTES[0]);
    if light { row.3 } else { row.2 }
}
