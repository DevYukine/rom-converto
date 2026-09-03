//! Synthetic TPL, BRLYT and BRLAN byte builders for the banner tests.

#![cfg(test)]

/// A material with one texture map, no TEV stages and an explicit material
/// colour, which is the shape almost every real banner material takes.
#[derive(Clone, Debug)]
pub(crate) struct MaterialSpec {
    pub name: String,
    pub texture_index: Option<u16>,
    /// Translation x/y, rotation in degrees, scale x/y.
    pub texture_srt: Option<[f32; 5]>,
    pub texcoord_gen_mtrx_src: Option<u8>,
    pub material_color: [u8; 4],
    pub fore: [i16; 4],
    pub back: [i16; 4],
}

impl Default for MaterialSpec {
    fn default() -> Self {
        Self {
            name: "mat".to_string(),
            texture_index: Some(0),
            texture_srt: None,
            texcoord_gen_mtrx_src: None,
            material_color: [255; 4],
            fore: [0; 4],
            back: [255; 4],
        }
    }
}

/// A `pic1` pane nested under the layout's root pane.
#[derive(Clone, Debug)]
pub(crate) struct PaneSpec {
    pub name: String,
    pub visible: bool,
    pub origin: u8,
    pub alpha: u8,
    pub translate: [f32; 3],
    pub width: f32,
    pub height: f32,
    pub material_index: u16,
    pub vertex_colors: [[u8; 4]; 4],
}

impl PaneSpec {
    pub(crate) fn picture(name: &str) -> Self {
        Self {
            name: name.to_string(),
            visible: true,
            origin: 4,
            alpha: 255,
            translate: [0.0; 3],
            width: 32.0,
            height: 16.0,
            material_index: 0,
            vertex_colors: [[255; 4]; 4],
        }
    }
}

pub(crate) struct AnimatorSpec {
    pub name: String,
    pub is_material: bool,
    pub tags: Vec<TagSpec>,
}

pub(crate) struct TagSpec {
    pub kind: u32,
    pub index: u8,
    pub target: u8,
    pub keys: KeySpec,
}

pub(crate) enum KeySpec {
    /// (frame, value)
    Step(Vec<(f32, u16)>),
    /// (frame, value, slope)
    Hermite(Vec<(f32, f32, f32)>),
}

impl TagSpec {
    pub(crate) fn step(kind: u32, index: u8, target: u8, keys: &[(f32, u16)]) -> Self {
        Self {
            kind,
            index,
            target,
            keys: KeySpec::Step(keys.to_vec()),
        }
    }

    pub(crate) fn hermite(kind: u32, index: u8, target: u8, keys: &[(f32, f32, f32)]) -> Self {
        Self {
            kind,
            index,
            target,
            keys: KeySpec::Hermite(keys.to_vec()),
        }
    }
}

/// Builds a single-image TPL. `palette` is (format, big-endian entries).
pub(crate) fn build_tpl(
    width: u32,
    height: u32,
    format: u32,
    pixels: &[u8],
    palette: Option<(u32, &[u8])>,
) -> Vec<u8> {
    const IMAGE_HEADER: usize = 0x14;
    const PALETTE_HEADER: usize = 0x38;
    const PALETTE_DATA: usize = 0x44;

    let palette_data_len = palette.map_or(0, |(_, data)| data.len());
    let pixel_offset = (PALETTE_DATA + palette_data_len).next_multiple_of(0x20);
    let mut out = vec![0u8; pixel_offset + pixels.len()];

    out[0..4].copy_from_slice(&0x0020_AF30u32.to_be_bytes());
    out[4..8].copy_from_slice(&1u32.to_be_bytes());
    out[8..12].copy_from_slice(&0x0Cu32.to_be_bytes());
    out[12..16].copy_from_slice(&(IMAGE_HEADER as u32).to_be_bytes());
    if palette.is_some() {
        out[16..20].copy_from_slice(&(PALETTE_HEADER as u32).to_be_bytes());
    }

    out[IMAGE_HEADER..IMAGE_HEADER + 2].copy_from_slice(&(height as u16).to_be_bytes());
    out[IMAGE_HEADER + 2..IMAGE_HEADER + 4].copy_from_slice(&(width as u16).to_be_bytes());
    out[IMAGE_HEADER + 4..IMAGE_HEADER + 8].copy_from_slice(&format.to_be_bytes());
    out[IMAGE_HEADER + 8..IMAGE_HEADER + 12].copy_from_slice(&(pixel_offset as u32).to_be_bytes());

    if let Some((palette_format, data)) = palette {
        let count = (data.len() / 2) as u16;
        out[PALETTE_HEADER..PALETTE_HEADER + 2].copy_from_slice(&count.to_be_bytes());
        out[PALETTE_HEADER + 4..PALETTE_HEADER + 8].copy_from_slice(&palette_format.to_be_bytes());
        out[PALETTE_HEADER + 8..PALETTE_HEADER + 12]
            .copy_from_slice(&(PALETTE_DATA as u32).to_be_bytes());
        out[PALETTE_DATA..PALETTE_DATA + data.len()].copy_from_slice(data);
    }
    out[pixel_offset..].copy_from_slice(pixels);
    out
}

/// Builds a CI8 TPL whose palette entries are RGB5A3.
pub(crate) fn build_tpl_ci8(width: u32, height: u32, pixels: &[u8], palette: &[u8]) -> Vec<u8> {
    build_tpl(width, height, 9, pixels, Some((2, palette)))
}

/// Builds a 4x4 RGB5A3 TPL filled with one colour.
pub(crate) fn build_solid_tpl(color: u16) -> Vec<u8> {
    build_tpl(4, 4, 5, &color.to_be_bytes().repeat(16), None)
}

/// Builds a BRLYT holding one `pan1` root named `root` with every pane in
/// `panes` nested beneath it as a `pic1`.
pub(crate) fn build_brlyt(
    width: f32,
    height: f32,
    textures: &[&str],
    materials: &[MaterialSpec],
    panes: &[PaneSpec],
) -> Vec<u8> {
    let mut sections: Vec<Vec<u8>> = Vec::new();

    let mut lyt = section(b"lyt1");
    lyt.extend_from_slice(&[1, 0, 0, 0]);
    lyt.extend_from_slice(&width.to_be_bytes());
    lyt.extend_from_slice(&height.to_be_bytes());
    sections.push(lyt);

    let mut txl = section(b"txl1");
    txl.extend_from_slice(&(textures.len() as u16).to_be_bytes());
    txl.extend_from_slice(&[0, 0]);
    let mut strings: Vec<u8> = Vec::new();
    for name in textures {
        let offset = textures.len() * 8 + strings.len();
        txl.extend_from_slice(&(offset as u32).to_be_bytes());
        txl.extend_from_slice(&[0; 4]);
        strings.extend_from_slice(name.as_bytes());
        strings.push(0);
    }
    txl.extend_from_slice(&strings);
    sections.push(txl);

    let mut mat = section(b"mat1");
    mat.extend_from_slice(&(materials.len() as u16).to_be_bytes());
    mat.extend_from_slice(&[0, 0]);
    let mut bodies: Vec<u8> = Vec::new();
    let mut offsets: Vec<u32> = Vec::new();
    for spec in materials {
        offsets.push((0x0C + materials.len() * 4 + bodies.len()) as u32);
        bodies.extend_from_slice(&build_material(spec));
    }
    for offset in offsets {
        mat.extend_from_slice(&offset.to_be_bytes());
    }
    mat.extend_from_slice(&bodies);
    sections.push(mat);

    let mut root = section(b"pan1");
    root.extend_from_slice(&pane_body(&PaneSpec {
        name: "root".to_string(),
        width,
        height,
        ..PaneSpec::picture("root")
    }));
    sections.push(root);

    sections.push(section(b"pas1"));
    for spec in panes {
        let mut pic = section(b"pic1");
        pic.extend_from_slice(&pane_body(spec));
        for color in spec.vertex_colors {
            pic.extend_from_slice(&color);
        }
        pic.extend_from_slice(&spec.material_index.to_be_bytes());
        pic.extend_from_slice(&[1, 0]);
        for uv in [[0.0f32, 0.0], [1.0, 0.0], [0.0, 1.0], [1.0, 1.0]] {
            pic.extend_from_slice(&uv[0].to_be_bytes());
            pic.extend_from_slice(&uv[1].to_be_bytes());
        }
        sections.push(pic);
    }
    sections.push(section(b"pae1"));

    finish(b"RLYT", sections)
}

/// [`build_brlyt`] plus trailing `grp1` sections of (group name, pane names).
pub(crate) fn build_brlyt_grouped(
    width: f32,
    height: f32,
    textures: &[&str],
    materials: &[MaterialSpec],
    panes: &[PaneSpec],
    groups: &[(&str, &[&str])],
) -> Vec<u8> {
    let mut out = build_brlyt(width, height, textures, materials, panes);
    let mut sections: Vec<Vec<u8>> = Vec::new();
    for (name, members) in groups {
        let mut grp = section(b"grp1");
        grp.extend_from_slice(&fixed(name, 16));
        grp.extend_from_slice(&(members.len() as u16).to_be_bytes());
        grp.extend_from_slice(&[0, 0]);
        for member in *members {
            grp.extend_from_slice(&fixed(member, 16));
        }
        sections.push(grp);
    }
    let section_count = u16::from_be_bytes([out[14], out[15]]) + sections.len() as u16;
    out[14..16].copy_from_slice(&section_count.to_be_bytes());
    for mut grp in sections {
        while !grp.len().is_multiple_of(4) {
            grp.push(0);
        }
        let size = grp.len() as u32;
        grp[4..8].copy_from_slice(&size.to_be_bytes());
        out.extend_from_slice(&grp);
    }
    let size = out.len() as u32;
    out[8..12].copy_from_slice(&size.to_be_bytes());
    out
}

/// Builds a BRLAN with one `pai1` section.
pub(crate) fn build_brlan(frame_count: u16, animators: &[AnimatorSpec]) -> Vec<u8> {
    let mut pai = section(b"pai1");
    pai.extend_from_slice(&frame_count.to_be_bytes());
    pai.extend_from_slice(&[1, 0]);
    pai.extend_from_slice(&0u16.to_be_bytes());
    pai.extend_from_slice(&(animators.len() as u16).to_be_bytes());
    pai.extend_from_slice(&0x14u32.to_be_bytes());

    let table = 0x14 + animators.len() * 4;
    let mut bodies: Vec<u8> = Vec::new();
    let mut offsets: Vec<u32> = Vec::new();
    for spec in animators {
        offsets.push((table + bodies.len()) as u32);
        bodies.extend_from_slice(&build_animator(spec));
    }
    for offset in offsets {
        pai.extend_from_slice(&offset.to_be_bytes());
    }
    pai.extend_from_slice(&bodies);

    finish(b"RLAN", vec![pai])
}

fn build_animator(spec: &AnimatorSpec) -> Vec<u8> {
    let mut out = fixed(&spec.name, 20);
    out.push(spec.tags.len() as u8);
    out.push(spec.is_material as u8);
    out.extend_from_slice(&[0, 0]);

    let table = 24 + spec.tags.len() * 4;
    let mut bodies: Vec<u8> = Vec::new();
    let mut offsets: Vec<u32> = Vec::new();
    for tag in &spec.tags {
        offsets.push((table + bodies.len()) as u32);
        bodies.extend_from_slice(&build_tag(tag));
    }
    for offset in offsets {
        out.extend_from_slice(&offset.to_be_bytes());
    }
    out.extend_from_slice(&bodies);
    out
}

fn build_tag(spec: &TagSpec) -> Vec<u8> {
    let mut entry = vec![spec.index, spec.target];
    let (data_type, key_count) = match &spec.keys {
        KeySpec::Step(keys) => (1u8, keys.len()),
        KeySpec::Hermite(keys) => (2u8, keys.len()),
    };
    entry.push(data_type);
    entry.push(0);
    entry.extend_from_slice(&(key_count as u16).to_be_bytes());
    entry.extend_from_slice(&[0, 0]);
    entry.extend_from_slice(&0x0Cu32.to_be_bytes());
    match &spec.keys {
        KeySpec::Step(keys) => {
            for (frame, value) in keys {
                entry.extend_from_slice(&frame.to_be_bytes());
                entry.extend_from_slice(&value.to_be_bytes());
                entry.extend_from_slice(&[0, 0]);
            }
        }
        KeySpec::Hermite(keys) => {
            for (frame, value, slope) in keys {
                entry.extend_from_slice(&frame.to_be_bytes());
                entry.extend_from_slice(&value.to_be_bytes());
                entry.extend_from_slice(&slope.to_be_bytes());
            }
        }
    }

    let mut out = spec.kind.to_be_bytes().to_vec();
    out.push(1);
    out.extend_from_slice(&[0, 0, 0]);
    out.extend_from_slice(&12u32.to_be_bytes());
    out.extend_from_slice(&entry);
    out
}

fn build_material(spec: &MaterialSpec) -> Vec<u8> {
    let mut out = fixed(&spec.name, 20);
    for reg in [spec.fore, spec.back, [0i16; 4]] {
        for channel in reg {
            out.extend_from_slice(&channel.to_be_bytes());
        }
    }
    out.extend_from_slice(&[0u8; 16]);

    let flags = spec.texture_index.is_some() as u32
        | ((spec.texture_srt.is_some() as u32) << 4)
        | ((spec.texcoord_gen_mtrx_src.is_some() as u32) << 8)
        | (1 << 27);
    out.extend_from_slice(&flags.to_be_bytes());

    if let Some(index) = spec.texture_index {
        out.extend_from_slice(&index.to_be_bytes());
        out.extend_from_slice(&[0, 0]);
    }
    if let Some(srt) = spec.texture_srt {
        for value in srt {
            out.extend_from_slice(&value.to_be_bytes());
        }
    }
    if let Some(mtrx_src) = spec.texcoord_gen_mtrx_src {
        out.extend_from_slice(&[0, 0, mtrx_src, 0]);
    }
    out.extend_from_slice(&spec.material_color);
    out
}

fn pane_body(spec: &PaneSpec) -> Vec<u8> {
    let mut out = vec![spec.visible as u8, spec.origin, spec.alpha, 0];
    out.extend_from_slice(&fixed(&spec.name, 16));
    out.extend_from_slice(&[0u8; 8]);
    for value in spec.translate {
        out.extend_from_slice(&value.to_be_bytes());
    }
    out.extend_from_slice(&[0u8; 12]);
    out.extend_from_slice(&1.0f32.to_be_bytes());
    out.extend_from_slice(&1.0f32.to_be_bytes());
    out.extend_from_slice(&spec.width.to_be_bytes());
    out.extend_from_slice(&spec.height.to_be_bytes());
    out
}

fn section(magic: &[u8; 4]) -> Vec<u8> {
    let mut out = magic.to_vec();
    out.extend_from_slice(&[0; 4]);
    out
}

fn finish(magic: &[u8; 4], sections: Vec<Vec<u8>>) -> Vec<u8> {
    let mut out = magic.to_vec();
    out.extend_from_slice(&0xFEFFu16.to_be_bytes());
    out.extend_from_slice(&0x0008u16.to_be_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&0x0010u16.to_be_bytes());
    out.extend_from_slice(&(sections.len() as u16).to_be_bytes());

    for mut body in sections {
        while !body.len().is_multiple_of(4) {
            body.push(0);
        }
        let size = body.len() as u32;
        body[4..8].copy_from_slice(&size.to_be_bytes());
        out.extend_from_slice(&body);
    }
    let total = out.len() as u32;
    out[8..12].copy_from_slice(&total.to_be_bytes());
    out
}

fn fixed(name: &str, len: usize) -> Vec<u8> {
    let mut out = vec![0u8; len];
    let bytes = name.as_bytes();
    let n = bytes.len().min(len);
    out[..n].copy_from_slice(&bytes[..n]);
    out
}
