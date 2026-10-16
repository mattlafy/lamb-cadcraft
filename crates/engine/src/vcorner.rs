//! Virtual corner hook (LAMB fork patch 10, not to be submitted): FILLET or CHAMFER on two
//! adjacent segments of one polyline asks a registered adapter first. When the adapter takes the
//! corner (`Ok(true)`) the vertex is left where it is and the adapter records the rounding its own
//! way (LAMB tags the vertex and draws the rounding live); `Ok(false)` falls through to CADCraft's
//! baked arc or chamfer line. A second, independent hook checks the two picks of a two-entity
//! FILLET on lines, arcs and circles before any fillet is attempted: `Err` refuses the command (LAMB
//! refuses two clicks nearer different corners of a curved pair). Unregistered, CADCraft is
//! unchanged.

use std::sync::OnceLock;

use cadcraft_doc::{Entity, Handle};
use cadcraft_geom::Vec2;

use crate::{Result, Session};

/// The corner operation requested at a vertex. A chamfer's `distance1` is the distance along the
/// segment that ends at the vertex, `distance2` along the one that starts there.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CornerOp {
    Fillet { radius: f64 },
    Chamfer { distance1: f64, distance2: f64 },
}

/// `(session, polyline, vertex index, op)` — `Ok(true)` when the adapter handled the corner.
pub type VirtualCornerHook = fn(&mut Session, Handle, usize, CornerOp) -> Result<bool>;

static HOOK: OnceLock<VirtualCornerHook> = OnceLock::new();

/// Register the virtual corner hook. Once per process; a second call is an error.
pub fn register_virtual_corner_hook(f: VirtualCornerHook) -> std::result::Result<(), &'static str> {
    HOOK.set(f).map_err(|_| "virtual corner hook already registered")
}

/// The registered virtual corner hook, if any.
pub fn virtual_corner_hook() -> Option<VirtualCornerHook> {
    HOOK.get().copied()
}

/// `(session, first entity, first pick, second entity, second pick)` — `Err` refuses the fillet.
pub type CornerPickCheck = fn(&Session, &Entity, Vec2, &Entity, Vec2) -> Result<()>;

static PICK_CHECK: OnceLock<CornerPickCheck> = OnceLock::new();

/// Register the two-entity FILLET pick check. Once per process; a second call is an error.
pub fn register_corner_pick_check(f: CornerPickCheck) -> std::result::Result<(), &'static str> {
    PICK_CHECK.set(f).map_err(|_| "corner pick check already registered")
}

/// The registered two-entity FILLET pick check, if any.
pub fn corner_pick_check() -> Option<CornerPickCheck> {
    PICK_CHECK.get().copied()
}
