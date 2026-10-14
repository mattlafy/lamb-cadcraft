//! Halos over the edited view's own linework (LAMB T351, TS `paths.ts` `HALO_COLOR`): RESOLVE's
//! attention halo on the current issue's entities ([`set_attention`], cleared when the command
//! ends), and the tentative glow on every unaccepted AI line (`EntityData.ai_origin.accepted ==
//! false`). Both are the entities rebuilt with [`cadcraft_render::build_entities`], recoloured,
//! translucent and at least [`HALO_MIN_LW`] wide; CADCraft's renderer is unchanged. An entity in
//! both draws the attention halo only (the TS rule).
//!
//! The attention state is session UI state, never saved; every change bumps
//! [`cadcraft_render::bump_overlays`], since the drawing's revision doesn't move.

use std::sync::Mutex;

use cadcraft_color::Rgb;
use cadcraft_doc::{Drawing, Entity, Handle, Space};
use cadcraft_geom::Bounds2;
use cadcraft_render::{DPrim, DisplayList, Kind, Options};

use super::view_id;
use crate::schema::read_entity_data;

/// TS `HALO_COLOR.attention`.
pub(crate) const ATTENTION_COLOR: Rgb = Rgb(0x3e, 0xa6, 0xff);
const ATTENTION_TRANSPARENCY: u8 = 40;
const TENTATIVE_TRANSPARENCY: u8 = 60;
/// Halo lineweight floor (mm), so a thin line still shows a wide halo.
const HALO_MIN_LW: f32 = 0.7;

/// The haloed block's name and the handles to halo in it.
static ATTENTION: Mutex<Option<(String, Vec<Handle>)>> = Mutex::new(None);

fn attention() -> std::sync::MutexGuard<'static, Option<(String, Vec<Handle>)>> {
    ATTENTION.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Halo `handles` in block `block` (RESOLVE's current issue), or nothing with `None`.
pub(crate) fn set_attention(to: Option<(String, Vec<Handle>)>) {
    *attention() = to;
    cadcraft_render::bump_overlays();
}

/// The currently haloed block and handles.
#[cfg(test)]
pub(crate) fn current_attention() -> Option<(String, Vec<Handle>)> {
    attention().clone()
}

fn tentative(e: &Entity) -> bool {
    read_entity_data(&e.common).ok().and_then(|d| d.ai_origin).is_some_and(|a| !a.accepted)
}

/// Append `src`'s prims to `out` recoloured (`None` keeps each prim's colour) and translucent.
fn append(out: &mut DisplayList, src: &DisplayList, color: Option<Rgb>, transparency: u8) {
    for prim in &src.prims {
        if prim.kind == Kind::Mask {
            continue;
        }
        let pts = src.points(prim);
        let buf = if prim.kind == Kind::Tris { &mut out.tris } else { &mut out.verts };
        let start = buf.len() as u32;
        buf.extend_from_slice(pts);
        if !matches!(prim.kind, Kind::Infinite { .. }) {
            for p in pts {
                out.bounds.add(*p);
            }
        }
        let (color, aci7) = match color {
            Some(c) => (c, false),
            None => (prim.color, prim.aci7),
        };
        out.prims.push(DPrim { color, aci7, transparency, lw: prim.lw.max(HALO_MIN_LW), start, len: pts.len() as u32, ..*prim });
    }
}

/// The halos for `space` when it is a LAMB view's block: the tentative glow first, then the
/// attention halo when RESOLVE is pointing into this block. Empty anywhere else.
pub(super) fn draw(d: &Drawing, space: &Space, opts: &Options) -> DisplayList {
    let mut out = DisplayList { bounds: Bounds2::EMPTY, ..DisplayList::default() };
    let Space::Block(name) = space else { return out };
    if view_id(space).is_none() {
        return out;
    }
    let Some(block) = d.block(name) else { return out };
    let opts = Options { view: None, ..opts.clone() };
    let haloed: Vec<Handle> = match &*attention() {
        Some((b, hs)) if b == name => hs.clone(),
        _ => Vec::new(),
    };
    let glow: Vec<&Entity> = block.entities.iter().map(|e| &**e).filter(|e| tentative(e) && !haloed.contains(&e.handle)).collect();
    if !glow.is_empty() {
        append(&mut out, &cadcraft_render::build_entities(d, glow, &opts), None, TENTATIVE_TRANSPARENCY);
    }
    if !haloed.is_empty() {
        let ents: Vec<&Entity> = block.entities.iter().map(|e| &**e).filter(|e| haloed.contains(&e.handle)).collect();
        append(&mut out, &cadcraft_render::build_entities(d, ents, &opts), Some(ATTENTION_COLOR), ATTENTION_TRANSPARENCY);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overlays::lamb_overlay;
    use crate::schema::{EntityData, set_entity_data, view_block_name};
    use cadcraft_engine::Session;
    use lamb_core::model::{AiOrigin, Id};
    use serde_json::json;

    type R = std::result::Result<(), Box<dyn std::error::Error>>;

    fn handles(s: &Session, block: &str) -> std::result::Result<Vec<Handle>, Box<dyn std::error::Error>> {
        Ok(s.doc()?.block(block).ok_or("no block")?.entities.iter().map(|e| e.handle).collect())
    }

    fn mark_ai(s: &mut Session, h: Handle, accepted: bool) -> R {
        let data = EntityData {
            ai_origin: Some(AiOrigin {
                proposal_id: "p1".into(),
                accepted,
                assumption_note: None,
                source_entity_id: None,
                swept_geometry: None,
                producer: None,
            }),
            ..EntityData::default()
        };
        let mut err = None;
        s.doc_mut()?.modify_entity(h, |e| err = set_entity_data(&mut e.common, &data).err())?;
        if let Some(e) = err {
            return Err(e.into());
        }
        Ok(())
    }

    #[test]
    fn unaccepted_ai_lines_glow_and_accepted_ones_do_not() -> R {
        let _g = crate::overlays::underlay::tests::lock();
        set_attention(None);
        crate::commands::install_app()?;
        let mut s = Session::new();
        s.new_drawing(true);
        let plan = Id(s.execute("lambproject", &json!({}))?["viewId"].as_u64().ok_or("no plan")?);
        let name = view_block_name(plan);
        let before: Vec<Handle> = handles(&s, &name)?;
        s.execute("line", &json!({ "points": [[0, 0], [1000, 0]] }))?;
        let h = *handles(&s, &name)?.iter().find(|h| !before.contains(h)).ok_or("no line")?;
        let space = Space::Block(name.clone());
        let base = lamb_overlay(s.doc()?, &space, &Options::default()).prims.len();

        mark_ai(&mut s, h, false)?;
        let list = lamb_overlay(s.doc()?, &space, &Options::default());
        let glow: Vec<&DPrim> = list.prims.iter().filter(|p| p.handle == h).collect();
        assert_eq!(glow.len(), 1, "{:?}", list.prims);
        assert_eq!(glow[0].transparency, TENTATIVE_TRANSPARENCY);
        assert!(glow[0].lw >= HALO_MIN_LW);
        assert_ne!(glow[0].color, ATTENTION_COLOR, "keeps its own colour");

        set_attention(Some((name.clone(), vec![h])));
        let list = lamb_overlay(s.doc()?, &space, &Options::default());
        let mine: Vec<&DPrim> = list.prims.iter().filter(|p| p.handle == h).collect();
        assert_eq!(mine.len(), 1, "both: attention only");
        assert_eq!((mine[0].color, mine[0].transparency), (ATTENTION_COLOR, ATTENTION_TRANSPARENCY));
        set_attention(None);

        mark_ai(&mut s, h, true)?;
        assert_eq!(lamb_overlay(s.doc()?, &space, &Options::default()).prims.len(), base, "accepted: no glow");
        Ok(())
    }
}
