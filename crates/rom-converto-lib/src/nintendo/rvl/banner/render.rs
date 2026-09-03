//! Software rasterizer that composes a parsed layout into an RGBA8 image.
//!
//! Layout space has its origin at the canvas centre with +y pointing up, so a
//! pane at (0, 0) lands in the middle of the output buffer.

use super::brlyt::{Layout, Material, Pane, TextureSrt};
use super::tpl::Texture;
use anyhow::{Result, anyhow};

const MAX_SIDE: f32 = 2048.0;

/// Renders `layout` at its declared canvas size over a transparent
/// background, returning the pixels and their dimensions.
///
/// `textures` is indexed by the layout's texture list; a `None` entry is
/// sampled as opaque white and still goes through the material combine.
pub(super) fn render(layout: &Layout, textures: &[Option<Texture>]) -> Result<(Vec<u8>, u32, u32)> {
    let width = layout.width.round();
    let height = layout.height.round();
    if !(1.0..=MAX_SIDE).contains(&width) || !(1.0..=MAX_SIDE).contains(&height) {
        return Err(anyhow!(
            "brlyt: implausible canvas size {}x{}",
            layout.width,
            layout.height
        ));
    }

    let mut target = Target {
        width: width as usize,
        height: height as usize,
        buf: vec![0u8; (width as usize) * (height as usize) * 4],
    };
    for pane in &layout.panes {
        draw_pane(&mut target, layout, textures, pane, Mat4::IDENTITY, 255.0);
    }
    Ok((target.buf, width as u32, height as u32))
}

struct Target {
    width: usize,
    height: usize,
    buf: Vec<u8>,
}

fn draw_pane(
    target: &mut Target,
    layout: &Layout,
    textures: &[Option<Texture>],
    pane: &Pane,
    parent: Mat4,
    parent_alpha: f32,
) {
    if !pane.visible {
        return;
    }
    let alpha = pane.alpha as f32;
    let render_alpha = if pane.influenced_alpha {
        parent_alpha * alpha / 255.0
    } else {
        alpha
    };
    let matrix = parent
        .mul(translation(pane.translate))
        .mul(rotation_x(pane.rotate[0]))
        .mul(rotation_y(pane.rotate[1]))
        .mul(rotation_z(pane.rotate[2]))
        .mul(scaling(pane.scale[0], pane.scale[1]));

    draw_quad(target, layout, textures, pane, matrix, render_alpha);
    for child in &pane.children {
        draw_pane(target, layout, textures, child, matrix, render_alpha);
    }
}

fn draw_quad(
    target: &mut Target,
    layout: &Layout,
    textures: &[Option<Texture>],
    pane: &Pane,
    matrix: Mat4,
    render_alpha: f32,
) {
    let Some(quad) = pane.quad.as_ref() else {
        return;
    };
    let Some(material) = layout.materials.get(quad.material_index as usize) else {
        return;
    };
    // Origin 0..8 walks the 3x3 anchor grid left-to-right, top-to-bottom;
    // clamp so a malformed value past 8 can't index outside the grid.
    let origin = pane.origin.min(8);
    let column = (origin % 3) as f32;
    let row = (origin / 3) as f32;
    let left = -pane.width * column * 0.5;
    let right = left + pane.width;
    let top = pane.height * row * 0.5;
    let bottom = top - pane.height;
    let corners = [[left, top], [right, top], [left, bottom], [right, bottom]];

    let uv = quad
        .uv
        .unwrap_or([[0.0, 0.0], [1.0, 0.0], [0.0, 1.0], [1.0, 1.0]]);

    let vertices: [Vertex; 4] = std::array::from_fn(|i| {
        let (x, y) = matrix.apply(corners[i][0], corners[i][1]);
        Vertex {
            x: target.width as f32 * 0.5 + x,
            y: target.height as f32 * 0.5 - y,
            uv: uv[i],
            color: quad.vertex_colors[i].map(|c| c as f32),
        }
    });

    let maps: Vec<(Option<&Texture>, u8, u8)> = material
        .texture_maps
        .iter()
        .map(|m| {
            (
                textures.get(m.index as usize).and_then(|t| t.as_ref()),
                m.wrap_s,
                m.wrap_t,
            )
        })
        .collect();
    let srts: Vec<Option<&TextureSrt>> = material
        .texcoord_gens
        .iter()
        .map(|coord_gen| {
            let index = (coord_gen.mtrx_src as i32 - 30) / 3;
            usize::try_from(index)
                .ok()
                .and_then(|i| material.texture_srts.get(i))
                // A non-finite value (e.g. from a divide-by-zero animated
                // scale) would otherwise NaN-poison every sampled texel; fall
                // back to the identity transform for that texcoord gen.
                .filter(|srt| {
                    [srt.tx, srt.ty, srt.rotate, srt.sx, srt.sy]
                        .iter()
                        .all(|v| v.is_finite())
                })
        })
        .collect();

    let shader = Shader {
        material,
        maps,
        srts,
        render_alpha,
    };
    fill_triangle(
        target,
        &shader,
        [&vertices[0], &vertices[1], &vertices[2]],
        false,
    );
    fill_triangle(
        target,
        &shader,
        [&vertices[1], &vertices[3], &vertices[2]],
        true,
    );
}

struct Vertex {
    x: f32,
    y: f32,
    uv: [f32; 2],
    color: [f32; 4],
}

/// `exclude_shared_edge` makes the `w1` weight (the a-c edge) use a strict
/// `> 0.0` test instead of `>= 0.0`. The quad's two triangles share a
/// diagonal edge; without this, a pixel exactly on that diagonal passes both
/// triangles' inclusive test and gets blended twice.
fn fill_triangle(
    target: &mut Target,
    shader: &Shader,
    tri: [&Vertex; 3],
    exclude_shared_edge: bool,
) {
    let [a, b, c] = tri;
    let area = edge(a.x, a.y, b.x, b.y, c.x, c.y);
    if area.abs() < 1e-6 {
        return;
    }
    let min_x = a.x.min(b.x).min(c.x).floor().max(0.0) as usize;
    let min_y = a.y.min(b.y).min(c.y).floor().max(0.0) as usize;
    let max_x = (a.x.max(b.x).max(c.x).ceil().max(0.0) as usize).min(target.width);
    let max_y = (a.y.max(b.y).max(c.y).ceil().max(0.0) as usize).min(target.height);

    for y in min_y..max_y {
        for x in min_x..max_x {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            // Dividing by the signed area keeps this correct for either winding.
            let w0 = edge(b.x, b.y, c.x, c.y, px, py) / area;
            let w1 = edge(c.x, c.y, a.x, a.y, px, py) / area;
            let w2 = edge(a.x, a.y, b.x, b.y, px, py) / area;
            let w1_inside = if exclude_shared_edge {
                w1 > 0.0
            } else {
                w1 >= 0.0
            };
            if w0 < 0.0 || !w1_inside || w2 < 0.0 {
                continue;
            }
            let uv = [
                a.uv[0] * w0 + b.uv[0] * w1 + c.uv[0] * w2,
                a.uv[1] * w0 + b.uv[1] * w1 + c.uv[1] * w2,
            ];
            let color =
                std::array::from_fn(|i| a.color[i] * w0 + b.color[i] * w1 + c.color[i] * w2);
            let src = shader.shade(uv, color);
            let off = (y * target.width + x) * 4;
            blend(
                &mut target.buf[off..off + 4],
                src,
                shader.material.blend_type,
            );
        }
    }
}

#[inline]
fn edge(ax: f32, ay: f32, bx: f32, by: f32, px: f32, py: f32) -> f32 {
    (bx - ax) * (py - ay) - (by - ay) * (px - ax)
}

fn blend(dst: &mut [u8], src: [f32; 4], blend_type: u8) {
    let src_alpha = src[3].clamp(0.0, 255.0);
    if blend_type == 0 {
        // GX blend "none" replaces the framebuffer pixel outright; the
        // written alpha never gates the write and the display ignores it, so
        // the result is opaque.
        for (d, s) in dst.iter_mut().take(3).zip(src) {
            *d = s.clamp(0.0, 255.0).round() as u8;
        }
        dst[3] = 255;
        return;
    }
    if src_alpha <= 0.0 {
        return;
    }
    let sa = src_alpha / 255.0;
    let da = dst[3] as f32 / 255.0;
    let out_alpha = sa + da * (1.0 - sa);
    if out_alpha <= 0.0 {
        return;
    }
    for i in 0..3 {
        let c = (src[i].clamp(0.0, 255.0) * sa + dst[i] as f32 * da * (1.0 - sa)) / out_alpha;
        dst[i] = c.clamp(0.0, 255.0).round() as u8;
    }
    dst[3] = (out_alpha * 255.0).round() as u8;
}

struct Shader<'a> {
    material: &'a Material,
    /// Texture, wrap_s and wrap_t per material texture map.
    maps: Vec<(Option<&'a Texture>, u8, u8)>,
    /// Resolved SRT per material texcoord gen.
    srts: Vec<Option<&'a TextureSrt>>,
    render_alpha: f32,
}

impl Shader<'_> {
    fn shade(&self, uv: [f32; 2], vertex_color: [f32; 4]) -> [f32; 4] {
        let material = self.material;
        let material_color = material.material_color.map(|c| c as f32);
        let rgb = if material.diffuse_src == 1 {
            vertex_color
        } else {
            material_color
        };
        let alpha = if material.alpha_src == 1 {
            vertex_color[3]
        } else {
            material_color[3]
        };
        let raster = [rgb[0], rgb[1], rgb[2], alpha * self.render_alpha / 255.0];
        self.combine(uv, raster)
    }

    /// Samples texture map `map` through texcoord gen `coord`'s SRT; a
    /// missing map or texture samples as opaque white.
    fn sample_map(&self, map: usize, coord: usize, uv: [f32; 2]) -> [f32; 4] {
        let Some(&(Some(texture), wrap_s, wrap_t)) = self.maps.get(map) else {
            return [255.0; 4];
        };
        let uv = match self.srts.get(coord).copied().flatten() {
            Some(srt) => transform_uv(srt, uv),
            None => uv,
        };
        sample(texture, uv, wrap_s, wrap_t)
    }

    /// Runs the material's TEV stages, or the fixed fore/back blend a
    /// stageless material implies. Each stage samples its own texture map.
    fn combine(&self, uv: [f32; 2], raster: [f32; 4]) -> [f32; 4] {
        let material = self.material;
        let regs: [[f32; 4]; 3] = material
            .color_regs
            .map(|reg| reg.map(|c| c.clamp(0, 255) as f32));
        if material.tev_stages.is_empty() {
            let texel = self.sample_map(0, 0, uv);
            let (fore, back) = (regs[0], regs[1]);
            return std::array::from_fn(|i| {
                raster[i] * lerp(fore[i], back[i], texel[i] / 255.0) / 255.0
            });
        }

        let konst = material.konst[0].map(|c| c as f32);
        let mut prev = [0.0f32; 4];
        for stage in &material.tev_stages {
            let texel = self.sample_map(stage.tex_map as usize, stage.tex_coord as usize, uv);
            let mut out = [0.0f32; 4];
            let inputs: [[f32; 3]; 4] = stage
                .color
                .map(|sel| color_input(sel, texel, raster, &regs, konst, prev));
            for i in 0..3 {
                out[i] = (inputs[3][i] + lerp(inputs[0][i], inputs[1][i], inputs[2][i] / 255.0))
                    .clamp(0.0, 255.0);
            }
            let alphas = stage
                .alpha
                .map(|sel| alpha_input(sel, texel, raster, &regs, konst, prev));
            out[3] = (alphas[3] + lerp(alphas[0], alphas[1], alphas[2] / 255.0)).clamp(0.0, 255.0);
            prev = out;
        }
        prev
    }
}

fn color_input(
    sel: u8,
    texel: [f32; 4],
    raster: [f32; 4],
    regs: &[[f32; 4]; 3],
    konst: [f32; 4],
    prev: [f32; 4],
) -> [f32; 3] {
    match sel {
        0 => [prev[0], prev[1], prev[2]],
        1 => [prev[3]; 3],
        2 => [regs[0][0], regs[0][1], regs[0][2]],
        3 => [regs[0][3]; 3],
        4 => [regs[1][0], regs[1][1], regs[1][2]],
        5 => [regs[1][3]; 3],
        6 => [regs[2][0], regs[2][1], regs[2][2]],
        7 => [regs[2][3]; 3],
        8 => [texel[0], texel[1], texel[2]],
        9 => [texel[3]; 3],
        10 => [raster[0], raster[1], raster[2]],
        11 => [raster[3]; 3],
        12 => [255.0; 3],
        13 => [127.5; 3],
        14 => [konst[0], konst[1], konst[2]],
        _ => [0.0; 3],
    }
}

fn alpha_input(
    sel: u8,
    texel: [f32; 4],
    raster: [f32; 4],
    regs: &[[f32; 4]; 3],
    konst: [f32; 4],
    prev: [f32; 4],
) -> f32 {
    match sel {
        0 => prev[3],
        1 => regs[0][3],
        2 => regs[1][3],
        3 => regs[2][3],
        4 => texel[3],
        5 => raster[3],
        6 => konst[3],
        _ => 0.0,
    }
}

#[inline]
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn transform_uv(srt: &TextureSrt, uv: [f32; 2]) -> [f32; 2] {
    let shift_x = if srt.sx != 0.0 { srt.tx / srt.sx } else { 0.0 };
    let shift_y = if srt.sy != 0.0 { srt.ty / srt.sy } else { 0.0 };
    let x = (uv[0] + shift_x - 0.5) * srt.sx;
    let y = (uv[1] + shift_y - 0.5) * srt.sy;
    let (sin, cos) = srt.rotate.to_radians().sin_cos();
    [x * cos - y * sin + 0.5, x * sin + y * cos + 0.5]
}

fn sample(texture: &Texture, uv: [f32; 2], wrap_s: u8, wrap_t: u8) -> [f32; 4] {
    let x = uv[0] * texture.width as f32 - 0.5;
    let y = uv[1] * texture.height as f32 - 0.5;
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);

    let xs = [
        wrap(x0 as i64, texture.width as i64, wrap_s),
        wrap(x0 as i64 + 1, texture.width as i64, wrap_s),
    ];
    let ys = [
        wrap(y0 as i64, texture.height as i64, wrap_t),
        wrap(y0 as i64 + 1, texture.height as i64, wrap_t),
    ];

    let mut out = [0.0f32; 4];
    for (j, &ty) in ys.iter().enumerate() {
        let wy = if j == 0 { 1.0 - fy } else { fy };
        for (i, &tx) in xs.iter().enumerate() {
            let wx = if i == 0 { 1.0 - fx } else { fx };
            let texel = texture.texel(tx, ty);
            for (channel, value) in out.iter_mut().zip(texel) {
                *channel += value as f32 * wx * wy;
            }
        }
    }
    out
}

fn wrap(coord: i64, size: i64, mode: u8) -> usize {
    let wrapped = match mode {
        1 => coord.rem_euclid(size),
        2 => {
            let period = coord.rem_euclid(size * 2);
            if period < size {
                period
            } else {
                size * 2 - 1 - period
            }
        }
        _ => coord.clamp(0, size - 1),
    };
    wrapped as usize
}

#[derive(Clone, Copy)]
struct Mat4([[f32; 4]; 4]);

impl Mat4 {
    const IDENTITY: Mat4 = Mat4([
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]);

    fn mul(self, rhs: Mat4) -> Mat4 {
        let mut out = [[0.0f32; 4]; 4];
        for (r, row) in out.iter_mut().enumerate() {
            for (c, cell) in row.iter_mut().enumerate() {
                *cell = (0..4).map(|k| self.0[r][k] * rhs.0[k][c]).sum();
            }
        }
        Mat4(out)
    }

    /// Projects a point on the z = 0 plane, dropping z orthographically.
    fn apply(&self, x: f32, y: f32) -> (f32, f32) {
        (
            self.0[0][0] * x + self.0[0][1] * y + self.0[0][3],
            self.0[1][0] * x + self.0[1][1] * y + self.0[1][3],
        )
    }
}

fn translation(t: [f32; 3]) -> Mat4 {
    let mut m = Mat4::IDENTITY;
    m.0[0][3] = t[0];
    m.0[1][3] = t[1];
    m.0[2][3] = t[2];
    m
}

fn rotation_x(degrees: f32) -> Mat4 {
    let (sin, cos) = degrees.to_radians().sin_cos();
    let mut m = Mat4::IDENTITY;
    m.0[1][1] = cos;
    m.0[1][2] = -sin;
    m.0[2][1] = sin;
    m.0[2][2] = cos;
    m
}

fn rotation_y(degrees: f32) -> Mat4 {
    let (sin, cos) = degrees.to_radians().sin_cos();
    let mut m = Mat4::IDENTITY;
    m.0[0][0] = cos;
    m.0[0][2] = sin;
    m.0[2][0] = -sin;
    m.0[2][2] = cos;
    m
}

fn rotation_z(degrees: f32) -> Mat4 {
    let (sin, cos) = degrees.to_radians().sin_cos();
    let mut m = Mat4::IDENTITY;
    m.0[0][0] = cos;
    m.0[0][1] = -sin;
    m.0[1][0] = sin;
    m.0[1][1] = cos;
    m
}

fn scaling(sx: f32, sy: f32) -> Mat4 {
    let mut m = Mat4::IDENTITY;
    m.0[0][0] = sx;
    m.0[1][1] = sy;
    m
}
