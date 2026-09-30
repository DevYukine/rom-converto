//! Frontend ROM-directory names per console label.
//!
//! Each row of `CONSOLES` carries one column per [`FRONTENDS`] token: the
//! ROM folder name, verbatim from that frontend's own end-user
//! documentation, that the frontend expects beneath its ROM root for the
//! row's console. A console the documentation does not list has no
//! directory, and its cell is [`None`]. Where a frontend's documentation
//! lists several folders for one console (regional or version-dependent
//! aliases), the primary folder of the current documentation is recorded.
//!
//! Per-token documentation sources:
//!
//! * `adam`: ROM folder table in
//!   <https://github.com/eduardofilo/RG350_adam_image/wiki/En:-3.-Content-installation>
//! * `batocera`: <https://wiki.batocera.org/systems> (the system short
//!   name doubles as the folder under `/userdata/roms`), cross-checked on
//!   per-system pages such as
//!   <https://wiki.batocera.org/systems:megacd>
//! * `crossmix`: "Rom Folder (Case Sensitive)" tables on
//!   <https://github.com/cizia64/CrossMix-OS/wiki/Emulators>
//! * `es`: "Supported game systems" table of the user guide linked from
//!   <https://es-de.org/> (fetched as
//!   <https://gitlab.com/api/v4/projects/18817634/repository/files/USERGUIDE.md/raw?ref=master>;
//!   the System name column is the directory under the ROM root)
//! * `funkeyos`: <https://doc.funkey-project.com/user_manual/tutorials/software/add_roms/>
//!   and the default games archive on
//!   <https://wiki.funkey-project.com/wiki/FunKey_Wiki_Knowledge_Center>
//! * `minui`: README.txt shipped on the SD card in every release
//!   (<https://github.com/shauninman/MinUI/blob/main/skeleton/BASE/README.txt>);
//!   folders use its documented `<System> (TAG)` form, combining the
//!   system names it lists with the per-system tags from its Bios section
//! * `mister`: the games-folder convention in
//!   <https://mister-devel.github.io/MkDocs_MiSTer/setup/games/> plus the
//!   per-core pages under <https://github.com/MiSTer-devel> (each core
//!   README names its own folder)
//! * `miyoocfw`: ROM-location table on
//!   <https://miyoocfw.gznetwork.com/latest/Emulator-Info/> (source:
//!   <https://github.com/MiyooCFW/docs>)
//! * `onion`: folder reference list on
//!   <https://onionui.github.io/docs/emulators/folders>
//! * `pocket`: <https://www.analogue.co/developer/docs/openfpga/directories-and-sd-folder-structure>
//!   (`/Assets/<platform>/common`) and
//!   <https://www.analogue.co/developer/docs/openfpga/custom-palettes>;
//!   the developer documentation names only `gb` as a platform folder and
//!   leaves the identifiers to individual cores, so every other cell is
//!   [`None`]
//! * `retrodeck`: ROMs directory table on
//!   <https://retrodeck.readthedocs.io/en/latest/wiki_management/retrodeck-folders>
//! * `rocknix`: per-system "Game Path" tables under
//!   <https://rocknix.org/systems/>, e.g.
//!   <https://rocknix.org/systems/gba/>
//! * `romm`: folder name = platform slug in
//!   <https://docs.romm.app/latest/platforms/supported-platforms/> (layout
//!   described in
//!   <https://docs.romm.app/latest/getting-started/folder-structure/>)
//! * `spruce`: ROM folder chart on
//!   <https://github.com/spruceUI/spruceOS/wiki/11.-Adding-Games>
//! * `twmenu`: <https://wiki.ds-homebrew.com/twilightmenu/>; the
//!   documentation ships a single `roms` folder and documents no
//!   per-system subfolders, so every cell is [`None`]
//!
//! Tokens resolve to the empty string in templates for every console their
//! documentation does not list: `twmenu` documents a single `roms` folder,
//! and `pocket` documents only its Game Boy folder.

/// Frontend tokens, in the column order of the backing `CONSOLES` table.
pub const FRONTENDS: &[&str] = &[
    "adam",
    "batocera",
    "crossmix",
    "es",
    "funkeyos",
    "minui",
    "mister",
    "miyoocfw",
    "onion",
    "pocket",
    "retrodeck",
    "rocknix",
    "romm",
    "spruce",
    "twmenu",
];

/// `(console label, directory per frontend)` rows, columns in
/// [`FRONTENDS`] order. `None` where the frontend's documentation defines
/// no folder for that console.
static CONSOLES: &[(&str, [Option<&str>; 15])] = &[
    (
        "NES",
        [
            Some("FC"),
            Some("nes"),
            Some("FC"),
            Some("nes"),
            Some("NES"),
            Some("Nintendo Entertainment System (FC)"),
            Some("NES"),
            Some("NES"),
            Some("FC"),
            None,
            Some("nes"),
            Some("nes"),
            Some("nes"),
            Some("FC"),
            None,
        ],
    ),
    (
        "Famicom Disk System",
        [
            Some("FDS"),
            Some("fds"),
            Some("FDS"),
            Some("fds"),
            None,
            None,
            Some("NES"),
            Some("NES"),
            Some("FDS"),
            None,
            Some("fds"),
            Some("fds"),
            Some("fds"),
            Some("FDS"),
            None,
        ],
    ),
    (
        "SNES",
        [
            Some("SFC"),
            Some("snes"),
            Some("SFC"),
            Some("snes"),
            Some("SNES"),
            Some("SNES (SFC)"),
            Some("SNES"),
            Some("SNES"),
            Some("SFC"),
            None,
            Some("snes"),
            Some("snes"),
            Some("snes"),
            Some("SFC"),
            None,
        ],
    ),
    (
        "N64",
        [
            None,
            Some("n64"),
            Some("N64"),
            Some("n64"),
            None,
            None,
            Some("N64"),
            None,
            None,
            None,
            Some("n64"),
            Some("n64"),
            Some("n64"),
            Some("N64"),
            None,
        ],
    ),
    (
        "GameCube",
        [
            None,
            Some("gamecube"),
            None,
            Some("gc"),
            None,
            None,
            None,
            None,
            None,
            None,
            Some("gc"),
            Some("gamecube"),
            Some("ngc"),
            None,
            None,
        ],
    ),
    (
        "Wii",
        [
            None,
            Some("wii"),
            None,
            Some("wii"),
            None,
            None,
            None,
            None,
            None,
            None,
            Some("wii"),
            Some("wii"),
            Some("wii"),
            None,
            None,
        ],
    ),
    (
        "WiiU",
        [
            None,
            Some("wiiu"),
            None,
            Some("wiiu"),
            None,
            None,
            None,
            None,
            None,
            None,
            Some("wiiu"),
            Some("wiiu"),
            Some("wiiu"),
            None,
            None,
        ],
    ),
    (
        "Switch",
        [
            None,
            None,
            None,
            Some("switch"),
            None,
            None,
            None,
            None,
            None,
            None,
            Some("switch"),
            None,
            Some("switch"),
            None,
            None,
        ],
    ),
    (
        "Game Boy",
        [
            Some("GB"),
            Some("gb"),
            Some("GB"),
            Some("gb"),
            Some("Game Boy"),
            Some("Game Boy (GB)"),
            Some("Gameboy"),
            Some("GB"),
            Some("GB"),
            Some("gb"),
            Some("gb"),
            Some("gb"),
            Some("gb"),
            Some("GB"),
            None,
        ],
    ),
    (
        "Game Boy Color",
        [
            Some("GBC"),
            Some("gbc"),
            Some("GBC"),
            Some("gbc"),
            Some("Game Boy Color"),
            Some("Game Boy Color (GBC)"),
            Some("Gameboy"),
            Some("GB"),
            Some("GBC"),
            None,
            Some("gbc"),
            Some("gbc"),
            Some("gbc"),
            Some("GBC"),
            None,
        ],
    ),
    (
        "Game Boy Advance",
        [
            Some("GBA"),
            Some("gba"),
            Some("GBA"),
            Some("gba"),
            Some("Game Boy Advance"),
            Some("Game Boy Advance (GBA)"),
            Some("GBA"),
            Some("GBA"),
            Some("GBA"),
            None,
            Some("gba"),
            Some("gba"),
            Some("gba"),
            Some("GBA"),
            None,
        ],
    ),
    (
        "NDS",
        [
            None,
            Some("nds"),
            Some("NDS"),
            Some("nds"),
            None,
            None,
            None,
            None,
            Some("NDS"),
            None,
            Some("nds"),
            Some("nds"),
            Some("nds"),
            Some("NDS"),
            None,
        ],
    ),
    (
        "3DS",
        [
            None,
            Some("3ds"),
            None,
            Some("n3ds"),
            None,
            None,
            None,
            None,
            None,
            None,
            Some("n3ds"),
            Some("3ds"),
            Some("3ds"),
            None,
            None,
        ],
    ),
    (
        "Virtual Boy",
        [
            Some("VB"),
            Some("virtualboy"),
            Some("VB"),
            Some("virtualboy"),
            None,
            None,
            None,
            None,
            Some("VB"),
            None,
            Some("virtualboy"),
            Some("virtualboy"),
            Some("virtualboy"),
            Some("VB"),
            None,
        ],
    ),
    (
        "Master System",
        [
            Some("SMS"),
            Some("mastersystem"),
            Some("MS"),
            Some("mastersystem"),
            Some("Sega Master System"),
            None,
            Some("SMS"),
            Some("SMS"),
            Some("MS"),
            None,
            Some("mastersystem"),
            Some("mastersystem"),
            Some("sms"),
            Some("MS"),
            None,
        ],
    ),
    (
        "Game Gear",
        [
            Some("GG"),
            Some("gamegear"),
            Some("GG"),
            Some("gamegear"),
            Some("Game Gear"),
            None,
            Some("SMS"),
            Some("SMS"),
            Some("GG"),
            None,
            Some("gamegear"),
            Some("gamegear"),
            Some("gamegear"),
            Some("GG"),
            None,
        ],
    ),
    (
        "Mega Drive",
        [
            Some("MD"),
            Some("megadrive"),
            Some("MD"),
            Some("megadrive"),
            Some("Sega Genesis"),
            Some("Sega Genesis (MD)"),
            Some("MegaDrive"),
            Some("SMD"),
            Some("MD"),
            None,
            Some("megadrive"),
            Some("megadrive"),
            Some("genesis"),
            Some("MD"),
            None,
        ],
    ),
    (
        "32X",
        [
            Some("32X"),
            Some("sega32x"),
            Some("SEGA32X"),
            Some("sega32x"),
            None,
            None,
            Some("S32X"),
            Some("SMD"),
            Some("THIRTYTWOX"),
            None,
            Some("sega32x"),
            Some("sega32x"),
            Some("sega32"),
            Some("THIRTYTWOX"),
            None,
        ],
    ),
    (
        "Sega CD",
        [
            Some("SEGACD"),
            Some("megacd"),
            Some("SEGACD"),
            Some("segacd"),
            None,
            None,
            Some("MegaCD"),
            Some("SMD"),
            Some("SEGACD"),
            None,
            Some("segacd"),
            Some("segacd"),
            Some("segacd"),
            Some("SEGACD"),
            None,
        ],
    ),
    (
        "Saturn",
        [
            None,
            Some("saturn"),
            Some("SATURN"),
            Some("saturn"),
            None,
            None,
            Some("Saturn"),
            None,
            None,
            None,
            Some("saturn"),
            Some("saturn"),
            Some("saturn"),
            Some("SATURN"),
            None,
        ],
    ),
    (
        "Dreamcast",
        [
            None,
            Some("dreamcast"),
            Some("DC"),
            Some("dreamcast"),
            None,
            None,
            None,
            None,
            None,
            None,
            Some("dreamcast"),
            Some("dreamcast"),
            Some("dc"),
            Some("DC"),
            None,
        ],
    ),
    (
        "PS1",
        [
            Some("PS"),
            Some("psx"),
            Some("PS"),
            Some("psx"),
            Some("PS1"),
            Some("Sony PlayStation (PS)"),
            Some("PSX"),
            Some("PS1"),
            Some("PS"),
            None,
            Some("psx"),
            Some("psx"),
            Some("psx"),
            Some("PS"),
            None,
        ],
    ),
    (
        "PS2",
        [
            None,
            Some("ps2"),
            None,
            Some("ps2"),
            None,
            None,
            None,
            None,
            None,
            None,
            Some("ps2"),
            Some("ps2"),
            Some("ps2"),
            None,
            None,
        ],
    ),
    (
        "PS3",
        [
            None,
            Some("ps3"),
            None,
            Some("ps3"),
            None,
            None,
            None,
            None,
            None,
            None,
            Some("ps3"),
            Some("ps3"),
            Some("ps3"),
            None,
            None,
        ],
    ),
    (
        "PSP",
        [
            None,
            Some("psp"),
            Some("PSP"),
            Some("psp"),
            None,
            None,
            None,
            None,
            None,
            None,
            Some("psp"),
            Some("psp"),
            Some("psp"),
            Some("PSP"),
            None,
        ],
    ),
    (
        "Vita",
        [
            None,
            Some("psvita"),
            None,
            Some("psvita"),
            None,
            None,
            None,
            None,
            None,
            None,
            Some("psvita"),
            None,
            Some("psvita"),
            None,
            None,
        ],
    ),
    (
        "PS4",
        [
            None,
            Some("ps4"),
            None,
            Some("ps4"),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            Some("ps4"),
            None,
            None,
        ],
    ),
    (
        "PS5",
        [
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            Some("ps5"),
            None,
            None,
        ],
    ),
    (
        "Xbox",
        [
            None,
            Some("xbox"),
            None,
            Some("xbox"),
            None,
            None,
            None,
            None,
            None,
            None,
            Some("xbox"),
            Some("xbox"),
            Some("xbox"),
            None,
            None,
        ],
    ),
    (
        "Xbox 360",
        [
            None,
            Some("xbox360"),
            None,
            Some("xbox360"),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            Some("xbox360"),
            None,
            None,
        ],
    ),
    (
        "WonderSwan",
        [
            Some("WSC"),
            Some("wswan"),
            Some("WS"),
            Some("wonderswan"),
            Some("WonderSwan"),
            None,
            Some("WonderSwan"),
            Some("WSWAN"),
            Some("WS"),
            None,
            Some("wonderswan"),
            Some("wonderswan"),
            Some("wonderswan"),
            Some("WS"),
            None,
        ],
    ),
    (
        "WonderSwan Color",
        [
            Some("WSC"),
            Some("wswanc"),
            Some("WSC"),
            Some("wonderswancolor"),
            None,
            None,
            Some("WonderSwan"),
            Some("WSWAN"),
            Some("WS"),
            None,
            Some("wonderswancolor"),
            Some("wonderswancolor"),
            Some("wonderswan-color"),
            Some("WSC"),
            None,
        ],
    ),
    (
        "Neo Geo Pocket",
        [
            Some("NGP"),
            Some("ngp"),
            Some("NGP"),
            Some("ngp"),
            Some("Neo Geo Pocket"),
            None,
            Some("NGPC"),
            Some("NGP"),
            Some("NGP"),
            None,
            Some("ngp"),
            Some("ngp"),
            Some("neo-geo-pocket"),
            Some("NGP"),
            None,
        ],
    ),
    (
        "Neo Geo Pocket Color",
        [
            Some("NGP"),
            Some("ngpc"),
            Some("NGC"),
            Some("ngpc"),
            None,
            None,
            Some("NGPC"),
            Some("NGP"),
            Some("NGP"),
            None,
            Some("ngpc"),
            Some("ngpc"),
            Some("neo-geo-pocket-color"),
            Some("NGPC"),
            None,
        ],
    ),
    (
        "Lynx",
        [
            Some("LYNX"),
            Some("lynx"),
            Some("LYNX"),
            Some("atarilynx"),
            Some("Atari Lynx"),
            None,
            Some("AtariLynx"),
            Some("LYNX"),
            Some("LYNX"),
            None,
            Some("atarilynx"),
            Some("atarilynx"),
            Some("lynx"),
            Some("LYNX"),
            None,
        ],
    ),
    (
        "Atari 7800",
        [
            Some("A7800"),
            Some("atari7800"),
            Some("ATARI7800"),
            Some("atari7800"),
            None,
            None,
            Some("Atari7800"),
            Some("7800"),
            Some("SEVENTYEIGHTHUNDRED"),
            None,
            Some("atari7800"),
            Some("atari7800"),
            Some("atari7800"),
            Some("SEVENTYEIGHTHUNDRED"),
            None,
        ],
    ),
    (
        "LaserDisc",
        [
            Some("DAPHNE"),
            Some("daphne"),
            Some("DAPHNE"),
            Some("laserdisc"),
            None,
            None,
            None,
            None,
            None,
            None,
            Some("laserdisc"),
            Some("daphne"),
            None,
            None,
            None,
        ],
    ),
    (
        "Arcade",
        [
            Some("ARCADE"),
            Some("mame"),
            Some("MAME"),
            Some("arcade"),
            None,
            None,
            Some("mame"),
            Some("FBA"),
            Some("ARCADE"),
            None,
            Some("arcade"),
            Some("mame"),
            Some("arcade"),
            Some("ARCADE"),
            None,
        ],
    ),
];

/// Frontend folder name for a rom-converto console label, or `None` when
/// the frontend token is unknown or the frontend's documentation defines
/// no folder for that pairing. A DAT platform label that names a table row
/// under a different label resolves through that row, so a matched disc
/// image resolves the same folders its console row would.
pub fn frontend_dir(frontend: &str, console_label: &str) -> Option<&'static str> {
    let column = FRONTENDS.iter().position(|f| *f == frontend)?;
    let row = CONSOLES
        .iter()
        .find(|row| row.0 == console_label)
        .or_else(|| {
            DAT_PLATFORM_ALIASES
                .iter()
                .find(|(platform, _)| *platform == console_label)
                .and_then(|(_, label)| CONSOLES.iter().find(|row| row.0 == *label))
        })?;
    row.1[column]
}

/// DAT platform labels that name a [`CONSOLES`] row under a different
/// label. `frontend_dir` checks these when the label itself matches no
/// row.
static DAT_PLATFORM_ALIASES: &[(&str, &str)] = &[
    ("7800", "Atari 7800"),
    ("Atari Lynx", "Lynx"),
    ("DreamCast", "Dreamcast"),
    ("Family Computer Disk System", "Famicom Disk System"),
    ("Mega CD & Sega CD", "Sega CD"),
    ("Mega Drive - Genesis", "Mega Drive"),
    ("Master System - Mark III", "Master System"),
    ("NeoGeo Pocket", "Neo Geo Pocket"),
    ("NeoGeo Pocket Color", "Neo Geo Pocket Color"),
    ("New Nintendo 3DS", "3DS"),
    ("Nintendo 3DS", "3DS"),
    ("Nintendo 64", "N64"),
    ("Nintendo DS", "NDS"),
    ("Nintendo DSi", "NDS"),
    ("Nintendo Entertainment System", "NES"),
    ("PlayStation", "PS1"),
    ("PlayStation 2", "PS2"),
    ("PlayStation 3", "PS3"),
    ("PlayStation 4", "PS4"),
    ("PlayStation 5", "PS5"),
    ("PlayStation Portable", "PSP"),
    ("PlayStation Vita", "Vita"),
    ("Sega Mega CD + Sega CD", "Sega CD"),
    ("Sega Saturn", "Saturn"),
    ("Super Nintendo Entertainment System", "SNES"),
    ("Wii U", "WiiU"),
];

/// Whether `token` names a supported frontend.
pub fn is_frontend(token: &str) -> bool {
    FRONTENDS.contains(&token)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spot checks against the cited documentation: one representative
    /// value (or documented absence) per frontend.
    #[test]
    fn spot_checks_match_documentation() {
        assert_eq!(frontend_dir("adam", "WonderSwan"), Some("WSC"));
        assert_eq!(frontend_dir("adam", "N64"), None);
        assert_eq!(frontend_dir("batocera", "Sega CD"), Some("megacd"));
        assert_eq!(frontend_dir("batocera", "WonderSwan Color"), Some("wswanc"));
        assert_eq!(frontend_dir("crossmix", "32X"), Some("SEGA32X"));
        assert_eq!(
            frontend_dir("crossmix", "Neo Geo Pocket Color"),
            Some("NGC")
        );
        assert_eq!(frontend_dir("es", "Game Boy Advance"), Some("gba"));
        assert_eq!(frontend_dir("es", "3DS"), Some("n3ds"));
        assert_eq!(frontend_dir("es", "PS4"), Some("ps4"));
        assert_eq!(frontend_dir("es", "PS5"), None);
        assert_eq!(frontend_dir("es", "LaserDisc"), Some("laserdisc"));
        assert_eq!(frontend_dir("funkeyos", "Mega Drive"), Some("Sega Genesis"));
        assert_eq!(frontend_dir("funkeyos", "Arcade"), None);
        assert_eq!(
            frontend_dir("minui", "NES"),
            Some("Nintendo Entertainment System (FC)")
        );
        assert_eq!(frontend_dir("minui", "Game Boy"), Some("Game Boy (GB)"));
        assert_eq!(
            frontend_dir("minui", "Game Boy Color"),
            Some("Game Boy Color (GBC)")
        );
        assert_eq!(
            frontend_dir("minui", "Mega Drive"),
            Some("Sega Genesis (MD)")
        );
        assert_eq!(frontend_dir("mister", "Mega Drive"), Some("MegaDrive"));
        assert_eq!(frontend_dir("mister", "Game Gear"), Some("SMS"));
        assert_eq!(frontend_dir("miyoocfw", "Atari 7800"), Some("7800"));
        assert_eq!(frontend_dir("miyoocfw", "Sega CD"), Some("SMD"));
        assert_eq!(frontend_dir("onion", "NES"), Some("FC"));
        assert_eq!(frontend_dir("onion", "Game Boy Advance"), Some("GBA"));
        assert_eq!(frontend_dir("pocket", "Game Boy"), Some("gb"));
        assert_eq!(frontend_dir("pocket", "SNES"), None);
        assert_eq!(frontend_dir("retrodeck", "3DS"), Some("n3ds"));
        assert_eq!(frontend_dir("retrodeck", "LaserDisc"), Some("laserdisc"));
        assert_eq!(frontend_dir("rocknix", "LaserDisc"), Some("daphne"));
        assert_eq!(frontend_dir("rocknix", "Switch"), None);
        assert_eq!(frontend_dir("romm", "Mega Drive"), Some("genesis"));
        assert_eq!(frontend_dir("romm", "32X"), Some("sega32"));
        assert_eq!(
            frontend_dir("spruce", "Atari 7800"),
            Some("SEVENTYEIGHTHUNDRED")
        );
        assert_eq!(frontend_dir("spruce", "Game Boy Color"), Some("GBC"));
        assert_eq!(frontend_dir("twmenu", "NDS"), None);
        assert_eq!(frontend_dir("twmenu", "NES"), None);
    }

    #[test]
    fn unknown_frontend_yields_none() {
        assert_eq!(frontend_dir("madeup", "NES"), None);
        assert!(!is_frontend("madeup"));
    }

    #[test]
    fn unknown_console_yields_none() {
        assert_eq!(frontend_dir("es", "Neo Geo CD"), None);
        assert_eq!(frontend_dir("romm", "Handheld Console"), None);
    }

    /// A DAT platform label that names a row under a different label
    /// resolves through that row, so a matched disc image gets the folders
    /// its console documents.
    #[test]
    fn dat_platform_labels_resolve_through_their_rows() {
        assert_eq!(frontend_dir("es", "Sega Saturn"), Some("saturn"));
        assert_eq!(frontend_dir("batocera", "PlayStation"), Some("psx"));
        assert_eq!(frontend_dir("romm", "Wii U"), Some("wiiu"));
        assert_eq!(frontend_dir("es", "Nintendo 64"), Some("n64"));
        assert_eq!(frontend_dir("es", "Arcade"), Some("arcade"));
    }

    /// Every folder name is non-empty, and every console label the
    /// detectors produce has a row, so a label renamed on either side
    /// cannot silently turn every frontend token empty.
    #[test]
    fn every_detected_console_label_has_a_row() {
        use crate::info::{DetectedConsole, console_label};
        use crate::util::template::retro_label_for_ext;

        for row in CONSOLES {
            assert!(
                row.1.iter().flatten().all(|dir| !dir.is_empty()),
                "{}",
                row.0
            );
        }
        let detected = [
            DetectedConsole::Ctr,
            DetectedConsole::Dol,
            DetectedConsole::Rvl,
            DetectedConsole::Wup,
            DetectedConsole::Nx,
            DetectedConsole::Xbox,
            DetectedConsole::Xenon,
            DetectedConsole::Ps3,
            DetectedConsole::Psp,
            DetectedConsole::LaserDisc,
            DetectedConsole::Ntr,
            DetectedConsole::Vpk,
            DetectedConsole::Ps4Pkg,
            DetectedConsole::Ps5Pkg,
        ]
        .into_iter()
        .filter_map(console_label);
        let cartridge = [
            "nes", "sfc", "z64", "gb", "gba", "md", "32x", "sms", "gg", "vb", "ws", "ngp", "lnx",
            "a78", "fds",
        ]
        .into_iter()
        .filter_map(retro_label_for_ext);
        let discs = ["PS1", "PS2", "Saturn", "Sega CD", "Dreamcast"];
        for label in detected.chain(cartridge).chain(discs) {
            assert!(
                CONSOLES.iter().any(|row| row.0 == label),
                "no frontend row for {label}"
            );
        }
    }
}
