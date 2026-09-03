//! BRLYT (`RLYT`) layout parser: canvas size, texture names, materials and
//! the pane tree.

use super::reader::{Reader, cstr};
use anyhow::{Result, anyhow};
use byteorder::{BE, ByteOrder};

/// One parsed layout file.
#[derive(Debug, Clone, Default)]
pub(super) struct Layout {
    pub width: f32,
    pub height: f32,
    pub textures: Vec<String>,
    pub materials: Vec<Material>,
    pub panes: Vec<Pane>,
    pub groups: Vec<Group>,
}

/// A `grp1` pane group; the System Menu shows and hides these per console
/// language.
#[derive(Debug, Clone)]
pub(super) struct Group {
    pub name: String,
    pub panes: Vec<String>,
}

/// A layout pane and its children.
#[derive(Debug, Clone)]
pub(super) struct Pane {
    pub visible: bool,
    /// False when the pane ignores its parent's alpha.
    pub influenced_alpha: bool,
    pub origin: u8,
    pub alpha: u8,
    pub name: String,
    pub translate: [f32; 3],
    /// Euler rotation in degrees.
    pub rotate: [f32; 3],
    pub scale: [f32; 2],
    pub width: f32,
    pub height: f32,
    pub quad: Option<Quad>,
    pub children: Vec<Pane>,
}

impl Pane {
    /// Writes an `RLPA` animation target: translate xyz, rotate xyz, scale xy,
    /// width, height.
    pub(super) fn set_target(&mut self, target: u8, value: f32) {
        match target {
            0..=2 => self.translate[target as usize] = value,
            3..=5 => self.rotate[target as usize - 3] = value,
            6..=7 => self.scale[target as usize - 6] = value,
            8 => self.width = value,
            9 => self.height = value,
            _ => {}
        }
    }
}

/// The drawn rectangle of a `pic1` pane or a `wnd1` pane's content.
///
/// Corners are ordered top-left, top-right, bottom-left, bottom-right.
#[derive(Debug, Clone)]
pub(super) struct Quad {
    pub vertex_colors: [[u8; 4]; 4],
    pub material_index: u16,
    pub uv: Option<[[f32; 2]; 4]>,
}

#[derive(Debug, Clone)]
pub(super) struct Material {
    pub name: String,
    /// TEV colour registers; index 0 is "fore", index 1 "back".
    pub color_regs: [[i16; 4]; 3],
    pub konst: [[u8; 4]; 4],
    pub texture_maps: Vec<TextureMap>,
    pub texture_srts: Vec<TextureSrt>,
    pub texcoord_gens: Vec<TexCoordGen>,
    /// 0 selects the material colour, 1 the interpolated vertex colour.
    pub diffuse_src: u8,
    pub alpha_src: u8,
    pub material_color: [u8; 4],
    pub tev_stages: Vec<TevStage>,
    pub blend_type: u8,
}

#[derive(Debug, Clone)]
pub(super) struct TextureMap {
    pub index: u16,
    pub wrap_s: u8,
    pub wrap_t: u8,
}

#[derive(Debug, Clone)]
pub(super) struct TextureSrt {
    pub tx: f32,
    pub ty: f32,
    pub rotate: f32,
    pub sx: f32,
    pub sy: f32,
}

impl TextureSrt {
    /// Writes an `RLTS` animation target: tx, ty, rotate, sx, sy.
    pub(super) fn set_target(&mut self, target: u8, value: f32) {
        match target {
            0 => self.tx = value,
            1 => self.ty = value,
            2 => self.rotate = value,
            3 => self.sx = value,
            4 => self.sy = value,
            _ => {}
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct TexCoordGen {
    pub mtrx_src: u8,
}

/// One TEV stage reduced to its texture bindings and its colour and alpha
/// combiner inputs `a, b, c, d`.
#[derive(Debug, Clone)]
pub(super) struct TevStage {
    /// Index into the material's texcoord gens (and their SRT matrices).
    pub tex_coord: u8,
    /// Index into the material's texture maps.
    pub tex_map: u16,
    pub color: [u8; 4],
    pub alpha: [u8; 4],
}

/// Hard cap on `pas1`/`pae1` nesting so a crafted layout can't build a pane
/// tree deep enough to blow the stack when [`super::render::render`] walks
/// it recursively. A `pas1` past this depth is skipped: its panes attach at
/// the current (capped) level instead of nesting further.
const MAX_PANE_DEPTH: usize = 64;

/// Parses a BRLYT file.
pub(super) fn parse(data: &[u8]) -> Result<Layout> {
    if data.len() < 0x10 || &data[..4] != b"RLYT" {
        return Err(anyhow!("brlyt: bad magic"));
    }
    let bom = BE::read_u16(&data[4..6]);
    if bom != 0xFEFF {
        return Err(anyhow!("brlyt: unexpected byte order mark 0x{:04X}", bom));
    }
    let version = BE::read_u16(&data[6..8]);
    if !matches!(version, 0x0008 | 0x000A) {
        return Err(anyhow!("brlyt: unsupported version 0x{:04X}", version));
    }
    let section_count = BE::read_u16(&data[14..16]) as usize;

    let mut layout = Layout::default();
    let mut levels: Vec<Vec<Pane>> = vec![Vec::new()];
    let mut parents: Vec<Option<Pane>> = Vec::new();
    // Counts `pas1` sections skipped for exceeding MAX_PANE_DEPTH, so their
    // matching `pae1` can be skipped too instead of closing a real, outer level.
    let mut capped_depth = 0usize;

    let mut pos = (BE::read_u16(&data[12..14]) as usize).max(0x10);
    for _ in 0..section_count {
        if pos + 8 > data.len() {
            break;
        }
        let magic = &data[pos..pos + 4];
        let size = BE::read_u32(&data[pos + 4..pos + 8]) as usize;
        if size < 8 || pos + size > data.len() {
            return Err(anyhow!(
                "brlyt: section at 0x{:X} declares {} bytes, past end",
                pos,
                size
            ));
        }
        let section = &data[pos..pos + size];
        match magic {
            b"lyt1" => {
                let mut r = Reader::at(section, 0x0C);
                layout.width = r.f32()?;
                layout.height = r.f32()?;
            }
            b"txl1" => layout.textures = parse_name_list(section)?,
            b"mat1" => layout.materials = parse_materials(section)?,
            b"pan1" | b"bnd1" | b"txt1" => push_pane(&mut levels, parse_pane(section, 8)?),
            b"pic1" => {
                let mut pane = parse_pane(section, 8)?;
                pane.quad = Some(parse_quad(section, 0x4C)?);
                push_pane(&mut levels, pane);
            }
            b"wnd1" => {
                let mut pane = parse_pane(section, 8)?;
                let mut r = Reader::at(section, 0x4C);
                // Frame inflation (4 floats) then frame count, flag and pad.
                r.skip(20)?;
                let content_offset = r.u32()? as usize;
                pane.quad = Some(parse_quad(section, content_offset)?);
                push_pane(&mut levels, pane);
            }
            b"pas1" => {
                if capped_depth > 0 || parents.len() >= MAX_PANE_DEPTH {
                    capped_depth += 1;
                } else {
                    // Push a level unconditionally, even when the current
                    // level has no pane to nest under (a `None` sentinel),
                    // so the matching `pae1` always pops what this `pas1`
                    // pushed instead of re-parenting an outer level.
                    let parent = levels.last_mut().and_then(|v| v.pop());
                    parents.push(parent);
                    levels.push(Vec::new());
                }
            }
            b"pae1" => {
                if capped_depth > 0 {
                    capped_depth -= 1;
                } else {
                    close_parent(&mut levels, &mut parents);
                }
            }
            b"grp1" => {
                let mut r = Reader::at(section, 8);
                let name = r.fixed_str(16)?;
                let count = r.u16()? as usize;
                r.skip(2)?;
                let mut panes = Vec::with_capacity(count);
                for _ in 0..count {
                    panes.push(r.fixed_str(16)?);
                }
                layout.groups.push(Group { name, panes });
            }
            _ => {}
        }
        pos += size;
    }

    while !parents.is_empty() {
        close_parent(&mut levels, &mut parents);
    }
    layout.panes = levels.into_iter().next().unwrap_or_default();
    Ok(layout)
}

fn push_pane(levels: &mut [Vec<Pane>], pane: Pane) {
    if let Some(level) = levels.last_mut() {
        level.push(pane);
    }
}

fn close_parent(levels: &mut Vec<Vec<Pane>>, parents: &mut Vec<Option<Pane>>) {
    let Some(slot) = parents.pop() else {
        return;
    };
    let children = levels.pop().unwrap_or_default();
    match slot {
        Some(mut parent) => {
            parent.children = children;
            push_pane(levels, parent);
        }
        // Sentinel: this `pas1` had no pane to nest under, so its children
        // surface as siblings of the enclosing level instead of being lost.
        None => {
            if let Some(level) = levels.last_mut() {
                level.extend(children);
            }
        }
    }
}

fn parse_pane(data: &[u8], off: usize) -> Result<Pane> {
    let mut r = Reader::at(data, off);
    let flags = r.u8()?;
    let origin = r.u8()?;
    let alpha = r.u8()?;
    r.skip(1)?;
    let name = r.fixed_str(16)?;
    r.skip(8)?;
    Ok(Pane {
        visible: flags & 1 != 0,
        influenced_alpha: (flags >> 1) & 1 == 0,
        origin,
        alpha,
        name,
        translate: [r.f32()?, r.f32()?, r.f32()?],
        rotate: [r.f32()?, r.f32()?, r.f32()?],
        scale: [r.f32()?, r.f32()?],
        width: r.f32()?,
        height: r.f32()?,
        quad: None,
        children: Vec::new(),
    })
}

fn parse_quad(data: &[u8], off: usize) -> Result<Quad> {
    let mut r = Reader::at(data, off);
    let mut vertex_colors = [[0u8; 4]; 4];
    for corner in vertex_colors.iter_mut() {
        corner.copy_from_slice(r.take(4)?);
    }
    let material_index = r.u16()?;
    let uv_set_count = r.u8()?;
    r.skip(1)?;
    let uv = if uv_set_count > 0 {
        let mut set = [[0f32; 2]; 4];
        for corner in set.iter_mut() {
            corner[0] = r.f32()?;
            corner[1] = r.f32()?;
        }
        Some(set)
    } else {
        None
    };
    Ok(Quad {
        vertex_colors,
        material_index,
        uv,
    })
}

/// `section` must be exactly the section's declared byte range (magic
/// through its size), so a truncated section errors instead of reading into
/// the next one.
fn parse_name_list(section: &[u8]) -> Result<Vec<String>> {
    let count = Reader::at(section, 8).u16()? as usize;
    let array = 0x0C;
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let offset = Reader::at(section, array + i * 8).u32()? as usize;
        out.push(cstr(section, array + offset)?);
    }
    Ok(out)
}

/// `section` must be exactly the section's declared byte range; see
/// [`parse_name_list`].
fn parse_materials(section: &[u8]) -> Result<Vec<Material>> {
    let count = Reader::at(section, 8).u16()? as usize;
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        // Unlike txl1's entries these offsets are packed, without a pad word.
        let offset = Reader::at(section, 0x0C + i * 4).u32()? as usize;
        out.push(parse_material(section, offset)?);
    }
    Ok(out)
}

fn parse_material(data: &[u8], off: usize) -> Result<Material> {
    let mut r = Reader::at(data, off);
    let name = r.fixed_str(20)?;
    let mut color_regs = [[0i16; 4]; 3];
    for reg in color_regs.iter_mut() {
        for channel in reg.iter_mut() {
            *channel = r.i16()?;
        }
    }
    let mut konst = [[0u8; 4]; 4];
    for color in konst.iter_mut() {
        color.copy_from_slice(r.take(4)?);
    }
    let flags = r.u32()?;

    let map_count = (flags & 0xF) as usize;
    let srt_count = ((flags >> 4) & 0xF) as usize;
    let gen_count = ((flags >> 8) & 0xF) as usize;
    let has_swap_table = (flags >> 12) & 1 == 1;
    let ind_srt_count = ((flags >> 13) & 0x3) as usize;
    let ind_stage_count = ((flags >> 15) & 0x7) as usize;
    let tev_count = ((flags >> 18) & 0x1F) as usize;
    let has_alpha_compare = (flags >> 23) & 1 == 1;
    let has_blend_mode = (flags >> 24) & 1 == 1;
    let has_channel_control = (flags >> 25) & 1 == 1;
    let has_material_color = (flags >> 27) & 1 == 1;

    let mut texture_maps = Vec::with_capacity(map_count);
    for _ in 0..map_count {
        texture_maps.push(TextureMap {
            index: r.u16()?,
            wrap_s: r.u8()?,
            wrap_t: r.u8()?,
        });
    }
    let mut texture_srts = Vec::with_capacity(srt_count);
    for _ in 0..srt_count {
        texture_srts.push(TextureSrt {
            tx: r.f32()?,
            ty: r.f32()?,
            rotate: r.f32()?,
            sx: r.f32()?,
            sy: r.f32()?,
        });
    }
    let mut texcoord_gens = Vec::with_capacity(gen_count);
    for _ in 0..gen_count {
        r.skip(2)?;
        texcoord_gens.push(TexCoordGen { mtrx_src: r.u8()? });
        r.skip(1)?;
    }
    let (diffuse_src, alpha_src) = if has_channel_control {
        let diffuse = r.u8()?;
        let alpha = r.u8()?;
        r.skip(2)?;
        (diffuse, alpha)
    } else {
        (1, 1)
    };
    let mut material_color = [0xFFu8; 4];
    if has_material_color {
        material_color.copy_from_slice(r.take(4)?);
    }
    if has_swap_table {
        r.skip(4)?;
    }
    r.skip(ind_srt_count * 20)?;
    r.skip(ind_stage_count * 4)?;

    let mut tev_stages = Vec::with_capacity(tev_count);
    for _ in 0..tev_count {
        let stage = r.take(16)?;
        // Combiner inputs pack the `a` and `c` selectors into the LOW nibble
        // of their byte (verified against retail materials whose only sane
        // reading is `lerp(fore, back, texel)`); tex_map sits in the low nine
        // bits of the u16 at +2.
        tev_stages.push(TevStage {
            tex_coord: stage[0],
            tex_map: BE::read_u16(&stage[2..4]) & 0x1FF,
            color: [stage[4] & 0xF, stage[4] >> 4, stage[5] & 0xF, stage[5] >> 4],
            alpha: [stage[8] & 0xF, stage[8] >> 4, stage[9] & 0xF, stage[9] >> 4],
        });
    }
    if has_alpha_compare {
        r.skip(4)?;
    }
    let blend_type = if has_blend_mode {
        let blend_type = r.u8()?;
        r.skip(3)?;
        blend_type
    } else {
        1
    };

    Ok(Material {
        name,
        color_regs,
        konst,
        texture_maps,
        texture_srts,
        texcoord_gens,
        diffuse_src,
        alpha_src,
        material_color,
        tev_stages,
        blend_type,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::rvl::banner::test_fixtures::{MaterialSpec, PaneSpec, build_brlyt};

    #[test]
    fn parses_canvas_textures_materials_and_pane_tree() {
        let brlyt = build_brlyt(
            64.0,
            32.0,
            &["tex.tpl"],
            &[MaterialSpec::default()],
            &[PaneSpec::picture("pic")],
        );
        let layout = parse(&brlyt).expect("brlyt must parse");

        assert_eq!((layout.width, layout.height), (64.0, 32.0));
        assert_eq!(layout.textures, vec!["tex.tpl".to_string()]);
        assert_eq!(layout.panes.len(), 1, "one root pan1");
        let root = &layout.panes[0];
        assert_eq!(root.name, "root");
        assert_eq!(root.children.len(), 1, "pas1/pae1 must nest the pic1");
        let pic = &root.children[0];
        assert_eq!(pic.name, "pic");
        assert!(pic.visible);
        assert!(pic.influenced_alpha);
        assert_eq!(pic.origin, 4);
        let quad = pic.quad.as_ref().expect("pic1 carries a quad");
        assert_eq!(quad.material_index, 0);
        assert_eq!(
            quad.uv,
            Some([[0.0, 0.0], [1.0, 0.0], [0.0, 1.0], [1.0, 1.0]])
        );
    }

    #[test]
    fn reads_material_blocks_in_flag_order() {
        // A texture SRT and a texcoord gen sit between the texture map and the
        // material colour, so a wrong read order shows up in every later field.
        let spec = MaterialSpec {
            texture_srt: Some([3.0, 4.0, 90.0, 2.0, 0.5]),
            texcoord_gen_mtrx_src: Some(30),
            material_color: [1, 2, 3, 4],
            ..MaterialSpec::default()
        };
        let brlyt = build_brlyt(8.0, 8.0, &["t.tpl"], &[spec], &[PaneSpec::picture("p")]);
        let layout = parse(&brlyt).expect("brlyt must parse");
        let material = &layout.materials[0];

        assert_eq!(material.name, "mat");
        assert_eq!(material.color_regs[0], [0, 0, 0, 0]);
        assert_eq!(material.color_regs[1], [255, 255, 255, 255]);
        assert_eq!(material.texture_maps.len(), 1);
        assert_eq!(material.texture_maps[0].index, 0);
        let srt = &material.texture_srts[0];
        assert_eq!(
            (srt.tx, srt.ty, srt.rotate, srt.sx, srt.sy),
            (3.0, 4.0, 90.0, 2.0, 0.5)
        );
        assert_eq!(material.texcoord_gens[0].mtrx_src, 30);
        assert_eq!(material.material_color, [1, 2, 3, 4]);
        assert_eq!(
            material.diffuse_src, 1,
            "no channel control defaults to vertex colour"
        );
        assert!(material.tev_stages.is_empty());
        assert_eq!(material.blend_type, 1);
    }

    #[test]
    fn rejects_a_bad_header() {
        let mut brlyt = build_brlyt(8.0, 8.0, &[], &[], &[]);
        brlyt[6] = 0xFF;
        assert!(
            parse(&brlyt).is_err(),
            "unsupported version must be rejected"
        );
    }

    /// A crafted BRLYT with far more `pas1`/`pae1` nesting than any real
    /// layout uses: MAX_PANE_DEPTH must cap the pane tree so parsing and the
    /// recursive renderer below it don't blow the stack.
    #[test]
    fn deeply_nested_pas1_is_capped_and_still_renders() {
        const DEPTH: usize = 300;

        fn raw_section(magic: &[u8; 4], body: &[u8]) -> Vec<u8> {
            let mut out = magic.to_vec();
            out.extend_from_slice(&((8 + body.len()) as u32).to_be_bytes());
            out.extend_from_slice(body);
            out
        }

        let mut lyt_body = vec![1u8, 0, 0, 0];
        lyt_body.extend_from_slice(&8.0f32.to_be_bytes());
        lyt_body.extend_from_slice(&8.0f32.to_be_bytes());

        let mut sections = vec![raw_section(b"lyt1", &lyt_body)];
        sections.extend((0..DEPTH).map(|_| raw_section(b"pas1", &[])));
        sections.extend((0..DEPTH).map(|_| raw_section(b"pae1", &[])));

        let mut brlyt = b"RLYT".to_vec();
        brlyt.extend_from_slice(&0xFEFFu16.to_be_bytes());
        brlyt.extend_from_slice(&0x0008u16.to_be_bytes());
        brlyt.extend_from_slice(&[0; 4]);
        brlyt.extend_from_slice(&0x0010u16.to_be_bytes());
        brlyt.extend_from_slice(&(sections.len() as u16).to_be_bytes());
        for section in &sections {
            brlyt.extend_from_slice(section);
        }
        let total = brlyt.len() as u32;
        brlyt[8..12].copy_from_slice(&total.to_be_bytes());

        let layout = parse(&brlyt).expect("deeply nested pas1/pae1 must still parse");
        assert!(
            layout.panes.is_empty(),
            "no pane ever owned a pas1 level in this fixture"
        );

        crate::nintendo::rvl::banner::render::render(&layout, &[])
            .expect("capped layout must still render");
    }
}
