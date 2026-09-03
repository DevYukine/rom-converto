//! BRLAN (`RLAN`) animation parser plus evaluation of a single frame onto a
//! parsed [`Layout`].

use super::brlyt::{Layout, Pane};
use super::reader::Reader;
use anyhow::{Result, anyhow};
use byteorder::{BE, ByteOrder};

const RLPA: u32 = u32::from_be_bytes(*b"RLPA");
const RLVI: u32 = u32::from_be_bytes(*b"RLVI");
const RLVC: u32 = u32::from_be_bytes(*b"RLVC");
const RLMC: u32 = u32::from_be_bytes(*b"RLMC");
const RLTS: u32 = u32::from_be_bytes(*b"RLTS");

/// One parsed animation file.
#[derive(Debug, Clone, Default)]
pub(super) struct Animation {
    pub frame_count: u16,
    pub animators: Vec<Animator>,
}

/// Every tag targeting one named pane or material.
#[derive(Debug, Clone)]
pub(super) struct Animator {
    pub name: String,
    pub is_material: bool,
    pub tags: Vec<Tag>,
}

/// A four-character-code group of animated entries.
#[derive(Debug, Clone)]
pub(super) struct Tag {
    pub kind: u32,
    pub entries: Vec<AnimEntry>,
}

/// One animated value: `index` selects the sub-object (a texture SRT, say),
/// `target` the field within it.
#[derive(Debug, Clone)]
pub(super) struct AnimEntry {
    pub index: u8,
    pub target: u8,
    pub keys: Keys,
}

#[derive(Debug, Clone)]
pub(super) enum Keys {
    Step(Vec<StepKey>),
    Hermite(Vec<HermiteKey>),
}

#[derive(Debug, Clone, Copy)]
pub(super) struct StepKey {
    pub frame: f32,
    pub value: u16,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct HermiteKey {
    pub frame: f32,
    pub value: f32,
    pub slope: f32,
}

/// Parses a BRLAN file.
pub(super) fn parse(data: &[u8]) -> Result<Animation> {
    if data.len() < 0x10 || &data[..4] != b"RLAN" {
        return Err(anyhow!("brlan: bad magic"));
    }
    let bom = BE::read_u16(&data[4..6]);
    if bom != 0xFEFF {
        return Err(anyhow!("brlan: unexpected byte order mark 0x{:04X}", bom));
    }
    let version = BE::read_u16(&data[6..8]);
    if !matches!(version, 0x0008 | 0x000A) {
        return Err(anyhow!("brlan: unsupported version 0x{:04X}", version));
    }
    let section_count = BE::read_u16(&data[14..16]) as usize;

    let mut animation = None;
    let mut pos = (BE::read_u16(&data[12..14]) as usize).max(0x10);
    for _ in 0..section_count {
        if pos + 8 > data.len() {
            break;
        }
        let magic = &data[pos..pos + 4];
        let size = BE::read_u32(&data[pos + 4..pos + 8]) as usize;
        if size < 8 || pos + size > data.len() {
            return Err(anyhow!(
                "brlan: section at 0x{:X} declares {} bytes, past end",
                pos,
                size
            ));
        }
        if magic == b"pai1" {
            animation = Some(parse_pai1(data, pos)?);
        }
        pos += size;
    }
    animation.ok_or_else(|| anyhow!("brlan: no pai1 section"))
}

fn parse_pai1(data: &[u8], section: usize) -> Result<Animation> {
    let mut r = Reader::at(data, section + 8);
    let frame_count = r.u16()?;
    r.skip(2)?;
    let _file_count = r.u16()?;
    let animator_count = r.u16()? as usize;
    let entry_offset = r.u32()? as usize;

    let mut animators = Vec::with_capacity(animator_count);
    for i in 0..animator_count {
        let offset = Reader::at(data, section + entry_offset + i * 4).u32()? as usize;
        animators.push(parse_animator(data, section + offset)?);
    }
    Ok(Animation {
        frame_count,
        animators,
    })
}

fn parse_animator(data: &[u8], origin: usize) -> Result<Animator> {
    let mut r = Reader::at(data, origin);
    let name = r.fixed_str(20)?;
    let tag_count = r.u8()? as usize;
    let is_material = r.u8()? != 0;
    r.skip(2)?;
    let mut offsets = Vec::with_capacity(tag_count);
    for _ in 0..tag_count {
        offsets.push(r.u32()? as usize);
    }
    let tags = offsets
        .into_iter()
        .map(|offset| parse_tag(data, origin + offset))
        .collect::<Result<Vec<_>>>()?;
    Ok(Animator {
        name,
        is_material,
        tags,
    })
}

fn parse_tag(data: &[u8], start: usize) -> Result<Tag> {
    let mut r = Reader::at(data, start);
    let kind = r.u32()?;
    let entry_count = r.u8()? as usize;
    r.skip(3)?;
    let mut offsets = Vec::with_capacity(entry_count);
    for _ in 0..entry_count {
        offsets.push(r.u32()? as usize);
    }
    let entries = offsets
        .into_iter()
        .map(|offset| parse_entry(data, start + offset))
        .collect::<Result<Vec<_>>>()?;
    Ok(Tag { kind, entries })
}

fn parse_entry(data: &[u8], off: usize) -> Result<AnimEntry> {
    let mut r = Reader::at(data, off);
    let index = r.u8()?;
    let target = r.u8()?;
    let data_type = r.u8()?;
    r.skip(1)?;
    let key_count = r.u16()? as usize;
    // The pad word is followed by an offset field that real files leave
    // pointing at the keys that already follow inline.
    r.skip(6)?;

    let keys = match data_type {
        1 => {
            let mut keys = Vec::with_capacity(key_count);
            for _ in 0..key_count {
                let frame = r.f32()?;
                let value = r.u16()?;
                r.skip(2)?;
                keys.push(StepKey { frame, value });
            }
            Keys::Step(keys)
        }
        2 => {
            let mut keys = Vec::with_capacity(key_count);
            for _ in 0..key_count {
                keys.push(HermiteKey {
                    frame: r.f32()?,
                    value: r.f32()?,
                    slope: r.f32()?,
                });
            }
            Keys::Hermite(keys)
        }
        other => return Err(anyhow!("brlan: unsupported key data type {}", other)),
    };
    Ok(AnimEntry {
        index,
        target,
        keys,
    })
}

/// Evaluates every animator at `frame` and writes the result into `layout`.
///
/// Panes and materials are matched by exact name; a target the layout does not
/// contain is skipped.
pub(super) fn apply(animation: &Animation, layout: &mut Layout, frame: f32) {
    for animator in &animation.animators {
        if animator.is_material {
            let Some(material) = layout
                .materials
                .iter_mut()
                .find(|m| m.name == animator.name)
            else {
                continue;
            };
            for tag in &animator.tags {
                for entry in &tag.entries {
                    let Some(value) = eval(entry, frame) else {
                        continue;
                    };
                    match tag.kind {
                        RLMC => match entry.target {
                            0..=3 => material.material_color[entry.target as usize] = to_u8(value),
                            4..=0xF => {
                                let t = (entry.target - 4) as usize;
                                material.color_regs[t / 4][t % 4] = value as i16;
                            }
                            0x10..=0x1F => {
                                let t = (entry.target - 0x10) as usize;
                                material.konst[t / 4][t % 4] = to_u8(value);
                            }
                            _ => {}
                        },
                        RLTS => {
                            if let Some(srt) = material.texture_srts.get_mut(entry.index as usize) {
                                srt.set_target(entry.target, value);
                            }
                        }
                        _ => {}
                    }
                }
            }
        } else {
            let Some(pane) = find_pane_mut(&mut layout.panes, &animator.name) else {
                continue;
            };
            for tag in &animator.tags {
                for entry in &tag.entries {
                    let Some(value) = eval(entry, frame) else {
                        continue;
                    };
                    match tag.kind {
                        RLPA => pane.set_target(entry.target, value),
                        RLVI => pane.visible = value != 0.0,
                        RLVC => match entry.target {
                            0..=0x0F => {
                                if let Some(quad) = pane.quad.as_mut() {
                                    let t = entry.target as usize;
                                    quad.vertex_colors[t / 4][t % 4] = to_u8(value);
                                }
                            }
                            0x10 => pane.alpha = to_u8(value),
                            _ => {}
                        },
                        _ => {}
                    }
                }
            }
        }
    }
}

fn to_u8(value: f32) -> u8 {
    value.clamp(0.0, 255.0).round() as u8
}

fn find_pane_mut<'a>(panes: &'a mut [Pane], name: &str) -> Option<&'a mut Pane> {
    for pane in panes.iter_mut() {
        if pane.name == name {
            return Some(pane);
        }
        if let Some(found) = find_pane_mut(&mut pane.children, name) {
            return Some(found);
        }
    }
    None
}

fn eval(entry: &AnimEntry, frame: f32) -> Option<f32> {
    match &entry.keys {
        Keys::Step(keys) => eval_step(keys, frame),
        Keys::Hermite(keys) => eval_hermite(keys, frame),
    }
}

fn eval_step(keys: &[StepKey], frame: f32) -> Option<f32> {
    let first = keys.first()?;
    let value = keys
        .iter()
        .take_while(|k| k.frame <= frame)
        .last()
        .unwrap_or(first)
        .value;
    Some(value as f32)
}

fn eval_hermite(keys: &[HermiteKey], frame: f32) -> Option<f32> {
    let first = keys.first()?;
    let last = keys.last()?;
    if frame <= first.frame {
        return Some(first.value);
    }
    if frame >= last.frame {
        return Some(last.value);
    }
    let i = keys.iter().rposition(|k| k.frame <= frame)?;
    let prev = keys[i];
    let Some(next) = keys.get(i + 1).copied() else {
        return Some(prev.value);
    };
    let span = next.frame - prev.frame;
    if span.abs() < 0.01 {
        return Some(prev.value);
    }
    let t = (frame - prev.frame) / span;
    let t2 = t * t;
    let t3 = t2 * t;
    Some(
        prev.slope * span * (t + t3 - 2.0 * t2)
            + next.slope * span * (t3 - t2)
            + prev.value * (1.0 + 2.0 * t3 - 3.0 * t2)
            + next.value * (-2.0 * t3 + 3.0 * t2),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::rvl::banner::brlyt;
    use crate::nintendo::rvl::banner::test_fixtures::{
        AnimatorSpec, MaterialSpec, PaneSpec, TagSpec, build_brlan, build_brlyt,
    };

    #[test]
    fn hermite_matches_the_reference_polynomial() {
        let keys = [
            HermiteKey {
                frame: 0.0,
                value: 0.0,
                slope: 0.0,
            },
            HermiteKey {
                frame: 10.0,
                value: 100.0,
                slope: 0.0,
            },
        ];
        // Zero slopes reduce the curve to smoothstep: at t = 0.5 that is half.
        assert_eq!(eval_hermite(&keys, 5.0), Some(50.0));
        // t = 0.25 -> 100 * (3t^2 - 2t^3) = 100 * (0.1875 - 0.03125).
        let v = eval_hermite(&keys, 2.5).expect("in range");
        assert!((v - 15.625).abs() < 1e-4, "got {}", v);
    }

    #[test]
    fn hermite_uses_the_slopes_and_clamps_outside_the_range() {
        let keys = [
            HermiteKey {
                frame: 0.0,
                value: 0.0,
                slope: 1.0,
            },
            HermiteKey {
                frame: 2.0,
                value: 0.0,
                slope: -1.0,
            },
        ];
        // t = 0.5, span = 2: 1*2*(0.5 + 0.125 - 0.5) + (-1)*2*(0.125 - 0.25) = 0.5.
        let v = eval_hermite(&keys, 1.0).expect("in range");
        assert!((v - 0.5).abs() < 1e-5, "got {}", v);
        assert_eq!(eval_hermite(&keys, -5.0), Some(0.0));
        assert_eq!(eval_hermite(&keys, 99.0), Some(0.0));
    }

    #[test]
    fn step_holds_the_last_key_at_or_before_the_frame() {
        let keys = [
            StepKey {
                frame: 2.0,
                value: 1,
            },
            StepKey {
                frame: 5.0,
                value: 0,
            },
        ];
        assert_eq!(eval_step(&keys, 0.0), Some(1.0), "clamps to the first key");
        assert_eq!(eval_step(&keys, 2.0), Some(1.0));
        assert_eq!(eval_step(&keys, 4.9), Some(1.0));
        assert_eq!(eval_step(&keys, 5.0), Some(0.0));
        assert_eq!(eval_step(&keys, 99.0), Some(0.0));
    }

    fn test_animation() -> Vec<u8> {
        build_brlan(
            10,
            &[AnimatorSpec {
                name: "pic".to_string(),
                is_material: false,
                tags: vec![
                    TagSpec::hermite(RLPA, 0, 0, &[(0.0, -8.0, 0.0), (10.0, 8.0, 0.0)]),
                    TagSpec::step(RLVI, 0, 0, &[(0.0, 1), (5.0, 0)]),
                ],
            }],
        )
    }

    #[test]
    fn step_value_above_255_round_trips_as_u16() {
        let brlan = build_brlan(
            10,
            &[AnimatorSpec {
                name: "pic".to_string(),
                is_material: false,
                tags: vec![TagSpec::step(RLPA, 0, 0, &[(0.0, 300)])],
            }],
        );
        let animation = parse(&brlan).expect("brlan must parse");
        match &animation.animators[0].tags[0].entries[0].keys {
            Keys::Step(keys) => assert_eq!(keys[0].value, 300, "a u8 read would truncate to 44"),
            other => panic!("expected step keys, got {:?}", other),
        }
    }

    #[test]
    fn parses_pane_animators_and_their_tags() {
        let animation = parse(&test_animation()).expect("brlan must parse");
        assert_eq!(animation.frame_count, 10);
        assert_eq!(animation.animators.len(), 1);
        let animator = &animation.animators[0];
        assert_eq!(animator.name, "pic");
        assert!(!animator.is_material);
        assert_eq!(animator.tags.len(), 2);
        assert_eq!(animator.tags[0].kind, RLPA);
        assert_eq!(animator.tags[1].kind, RLVI);
        match &animator.tags[1].entries[0].keys {
            Keys::Step(keys) => assert_eq!(keys.len(), 2),
            other => panic!("RLVI must carry step keys, got {:?}", other),
        }
    }

    #[test]
    fn applying_a_frame_moves_and_then_hides_the_pane() {
        let brlyt = build_brlyt(
            64.0,
            32.0,
            &["tex.tpl"],
            &[MaterialSpec::default()],
            &[PaneSpec::picture("pic")],
        );
        let animation = parse(&test_animation()).expect("brlan must parse");

        let mut layout = brlyt::parse(&brlyt).expect("brlyt must parse");
        apply(&animation, &mut layout, 0.0);
        assert_eq!(layout.panes[0].children[0].translate[0], -8.0);
        assert!(layout.panes[0].children[0].visible);

        let mut layout = brlyt::parse(&brlyt).expect("brlyt must parse");
        apply(&animation, &mut layout, 10.0);
        assert_eq!(layout.panes[0].children[0].translate[0], 8.0);
        assert!(
            !layout.panes[0].children[0].visible,
            "RLVI hides at frame 5"
        );
    }
}
