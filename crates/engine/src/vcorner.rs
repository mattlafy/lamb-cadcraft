//! Virtual corner hook (LAMB fork patch 10, not to be submitted): FILLET or CHAMFER on two
//! adjacent segments of one polyline asks a registered adapter first. When the adapter takes the
//! corner (`Ok(true)`) the vertex is left where it is and the adapter records the rounding its own
//! way (LAMB tags the vertex and draws the rounding live); `Ok(false)` falls through to CADCraft's
//! baked arc or chamfer line. Unregistered, CADCraft is unchanged.

use std::sync::OnceLock;

use cadcraft_doc::Handle;

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
