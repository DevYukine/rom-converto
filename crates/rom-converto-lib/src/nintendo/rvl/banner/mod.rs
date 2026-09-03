//! Static renderer for a Wii banner archive.
//!
//! A banner's `arc` tree pairs a BRLYT layout with the TPL textures it names
//! and a BRLAN animation. Composing them reproduces the frame the System Menu
//! shows, which is what [`render_banner`] returns; picking the largest texture
//! instead misses the composition entirely.
//!
//! The material combiner evaluates TEV stages in a reduced form: op ADD,
//! bias 0 and scale 1 are assumed, `konst[0]` stands in for every konst
//! reference, and swap tables plus indirect stages are ignored.

mod brlan;
mod brlyt;
mod reader;
mod render;
mod tpl;

#[cfg(test)]
pub(crate) mod test_fixtures;

use crate::nintendo::rvl::models::u8_archive::U8Archive;
use anyhow::{Context, Result, anyhow};
use tpl::Texture;

/// Renders the banner layout inside `inner` into RGBA8 pixels with their
/// width and height.
///
/// Textures the archive does not carry, or that fail to decode, are sampled
/// as opaque white and still pass through the material combine, rather than
/// failing the whole banner.
pub fn render_banner(inner: &U8Archive) -> Result<(Vec<u8>, u32, u32)> {
    let (path, bytes) = find_layout(inner)?;
    let mut layout = brlyt::parse(bytes).with_context(|| format!("banner: parse {}", path))?;

    let stem = file_name(&path);
    let stem = &stem[..stem.len() - ".brlyt".len()];
    // The System Menu plays the intro once, then loops. Targets only the
    // intro touches (a full-screen flash it fades out, say) keep its final
    // value, so the settled pose is the intro's last frame with the idle
    // loop's frame 0 layered on top.
    if let Some(animation) = load_animation(inner, stem, "_Start.brlan") {
        brlan::apply(&animation, &mut layout, animation.frame_count as f32);
    }
    if let Some(animation) =
        load_animation(inner, stem, "_Loop.brlan").or_else(|| load_animation(inner, stem, ".brlan"))
    {
        brlan::apply(&animation, &mut layout, 0.0);
    }
    hide_other_languages(&mut layout);

    let textures = load_textures(inner, &layout.textures);
    render::render(&layout, &textures).with_context(|| format!("banner: render {}", path))
}

fn find_layout<'a>(inner: &U8Archive<'a>) -> Result<(String, &'a [u8])> {
    let mut candidates: Vec<(String, &'a [u8])> = inner
        .list_paths()
        .into_iter()
        .filter(|(path, _)| path.to_ascii_lowercase().ends_with(".brlyt"))
        .collect();
    if candidates.is_empty() {
        return Err(anyhow!("banner: no .brlyt layout in the archive"));
    }
    // Prefer the canonical arc/blyt/ folder, then a file named "banner";
    // any remaining .brlyt is still usable if neither hint matches.
    let pick = candidates
        .iter()
        .position(|(path, _)| {
            let lower = path.to_ascii_lowercase();
            lower.contains("blyt") && file_name(&lower).contains("banner")
        })
        .or_else(|| {
            candidates
                .iter()
                .position(|(path, _)| path.to_ascii_lowercase().contains("blyt"))
        })
        .or_else(|| {
            candidates
                .iter()
                .position(|(path, _)| file_name(path).to_ascii_lowercase().contains("banner"))
        })
        .unwrap_or(0);
    Ok(candidates.swap_remove(pick))
}

fn load_textures(inner: &U8Archive, names: &[String]) -> Vec<Option<Texture>> {
    names
        .iter()
        .map(|name| {
            let Some(bytes) = inner.find_path_ending_with(&format!("/{}", name)) else {
                log::debug!("banner: texture {} not in the archive", name);
                return None;
            };
            match tpl::decode_first(bytes) {
                Ok(texture) => Some(texture),
                Err(e) => {
                    log::debug!("banner: texture {} skipped ({})", name, e);
                    None
                }
            }
        })
        .collect()
}

fn load_animation(inner: &U8Archive, stem: &str, suffix: &str) -> Option<brlan::Animation> {
    let path = format!("/{}{}", stem, suffix);
    let bytes = inner.find_path_ending_with(&path)?;
    match brlan::parse(bytes) {
        Ok(animation) => Some(animation),
        Err(e) => {
            log::debug!("banner: animation {} skipped ({})", path, e);
            None
        }
    }
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Group names the System Menu treats as language selectors.
const LANGUAGE_GROUPS: [&str; 12] = [
    "JPN", "ENG", "GER", "FRA", "SPA", "ITA", "NED", "POR", "RUS", "KOR", "CHN", "TWN",
];

/// Keeps one language group's panes (English when present, else the first
/// language group) and hides panes that belong only to the other languages,
/// the way the System Menu shows a single console language. Language panes
/// stack in the layout, so leaving them all visible garbles the text.
fn hide_other_languages(layout: &mut brlyt::Layout) {
    let languages: Vec<&brlyt::Group> = layout
        .groups
        .iter()
        .filter(|g| {
            LANGUAGE_GROUPS
                .iter()
                .any(|l| g.name.eq_ignore_ascii_case(l))
        })
        .collect();
    let Some(chosen) = languages
        .iter()
        .find(|g| g.name.eq_ignore_ascii_case("ENG"))
        .or_else(|| languages.first())
    else {
        return;
    };
    let hidden: Vec<String> = languages
        .iter()
        .flat_map(|g| g.panes.iter())
        .filter(|pane| !chosen.panes.contains(pane))
        .cloned()
        .collect();
    for pane in &mut layout.panes {
        hide_panes(pane, &hidden);
    }
}

fn hide_panes(pane: &mut brlyt::Pane, hidden: &[String]) {
    if hidden.iter().any(|h| h == &pane.name) {
        pane.visible = false;
    }
    for child in &mut pane.children {
        hide_panes(child, hidden);
    }
}

#[cfg(test)]
mod tests {
    use super::test_fixtures::*;
    use super::*;
    use crate::nintendo::rvl::test_fixtures::build_u8_archive;

    const RLPA: u32 = u32::from_be_bytes(*b"RLPA");
    const RLVI: u32 = u32::from_be_bytes(*b"RLVI");
    const RED: u16 = 0xFC00;

    fn inner_archive(panes: &[PaneSpec], animation: Option<Vec<u8>>) -> Vec<u8> {
        let brlyt = build_brlyt(64.0, 32.0, &["tex.tpl"], &[MaterialSpec::default()], panes);
        let mut entries = vec![
            ("arc/blyt/banner.brlyt", brlyt),
            ("arc/timg/tex.tpl", build_solid_tpl(RED)),
        ];
        if let Some(brlan) = animation {
            entries.push(("arc/anim/banner.brlan", brlan));
        }
        build_u8_archive(&entries)
    }

    fn pixel(rgba: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
        let off = ((y * width + x) * 4) as usize;
        [rgba[off], rgba[off + 1], rgba[off + 2], rgba[off + 3]]
    }

    #[test]
    fn renders_a_centered_textured_quad() {
        let data = inner_archive(&[PaneSpec::picture("pic")], None);
        let inner = U8Archive::parse(&data).expect("u8 archive must parse");
        let (rgba, width, height) = render_banner(&inner).expect("banner must render");

        assert_eq!((width, height), (64, 32));
        // The 32x16 quad is centred, so it covers x 16..48 and y 8..24.
        assert_eq!(pixel(&rgba, width, 32, 16), [255, 0, 0, 255]);
        assert_eq!(pixel(&rgba, width, 17, 9), [255, 0, 0, 255]);
        assert_eq!(pixel(&rgba, width, 0, 0), [0, 0, 0, 0]);
        assert_eq!(pixel(&rgba, width, 8, 16), [0, 0, 0, 0]);
    }

    #[test]
    fn animation_translates_the_quad_at_frame_zero() {
        let brlan = build_brlan(
            10,
            &[AnimatorSpec {
                name: "pic".to_string(),
                is_material: false,
                tags: vec![TagSpec::hermite(
                    RLPA,
                    0,
                    0,
                    &[(0.0, -16.0, 0.0), (10.0, 0.0, 0.0)],
                )],
            }],
        );
        let data = inner_archive(&[PaneSpec::picture("pic")], Some(brlan));
        let inner = U8Archive::parse(&data).expect("u8 archive must parse");
        let (rgba, width, _) = render_banner(&inner).expect("banner must render");

        // Shifted 16px left, the quad now covers x 0..32.
        assert_eq!(pixel(&rgba, width, 8, 16), [255, 0, 0, 255]);
        assert_eq!(pixel(&rgba, width, 40, 16), [0, 0, 0, 0]);
    }

    #[test]
    fn hidden_panes_are_not_drawn() {
        let brlan = build_brlan(
            10,
            &[AnimatorSpec {
                name: "pic".to_string(),
                is_material: false,
                tags: vec![TagSpec::step(RLVI, 0, 0, &[(0.0, 0)])],
            }],
        );
        let data = inner_archive(&[PaneSpec::picture("pic")], Some(brlan));
        let inner = U8Archive::parse(&data).expect("u8 archive must parse");
        let (rgba, _, _) = render_banner(&inner).expect("banner must render");
        assert!(
            rgba.iter().all(|&b| b == 0),
            "a hidden pane must draw nothing"
        );
    }

    #[test]
    fn vertex_colors_modulate_the_texture() {
        let pane = PaneSpec {
            vertex_colors: [[128, 255, 255, 255]; 4],
            ..PaneSpec::picture("pic")
        };
        let data = inner_archive(&[pane], None);
        let inner = U8Archive::parse(&data).expect("u8 archive must parse");
        let (rgba, width, _) = render_banner(&inner).expect("banner must render");
        assert_eq!(pixel(&rgba, width, 32, 16), [128, 0, 0, 255]);
    }

    #[test]
    fn missing_texture_renders_white() {
        let brlyt = build_brlyt(
            64.0,
            32.0,
            &["gone.tpl"],
            &[MaterialSpec::default()],
            &[PaneSpec::picture("pic")],
        );
        let data = build_u8_archive(&[("arc/blyt/banner.brlyt", brlyt)]);
        let inner = U8Archive::parse(&data).expect("u8 archive must parse");
        let (rgba, width, _) = render_banner(&inner).expect("banner must render");
        assert_eq!(pixel(&rgba, width, 32, 16), [255, 255, 255, 255]);
    }

    #[test]
    fn other_language_groups_are_hidden() {
        // ENG and GER share a background pane; only GER's own pane must hide.
        let brlyt = build_brlyt_grouped(
            64.0,
            32.0,
            &["tex.tpl"],
            &[MaterialSpec::default()],
            &[
                PaneSpec {
                    translate: [-16.0, 0.0, 0.0],
                    ..PaneSpec::picture("En")
                },
                PaneSpec {
                    translate: [16.0, 0.0, 0.0],
                    ..PaneSpec::picture("Ge")
                },
            ],
            &[("ENG", &["Bg", "En"]), ("GER", &["Bg", "Ge"])],
        );
        let data = build_u8_archive(&[
            ("arc/blyt/banner.brlyt", brlyt),
            ("arc/timg/tex.tpl", build_solid_tpl(RED)),
        ]);
        let inner = U8Archive::parse(&data).expect("u8 archive must parse");
        let (rgba, width, _) = render_banner(&inner).expect("banner must render");

        // The English pane (left) draws, the German pane (right) does not.
        assert_eq!(pixel(&rgba, width, 16, 16), [255, 0, 0, 255]);
        assert_eq!(pixel(&rgba, width, 48, 16), [0, 0, 0, 0]);
    }

    #[test]
    fn errors_when_there_is_no_layout() {
        let data = build_u8_archive(&[("arc/timg/tex.tpl", build_solid_tpl(RED))]);
        let inner = U8Archive::parse(&data).expect("u8 archive must parse");
        assert!(render_banner(&inner).is_err());
    }
}
