//! Closed cut shapes as capped solids (TS `solids.ts`). The stored form is faces, not triangles:
//! each face a planar closed loop with the first vertex not repeated, so a solid can be clipped
//! again; [`solid_triangles`] triangulates for the renderer.

use serde::{Deserialize, Serialize};

use super::projection::{vertical_basis, vertical_gaze_dir, vertical_local_to_global3};
use super::read::ViewRef;
use super::resolve::ProvenanceKind;
use crate::geometry::halfspace::{HalfSpace, clip_convex_polyhedron};
use crate::geometry::point::{add3, cross3, dot3, normalize3, scale3, sub3};
use crate::geometry::triangulate::triangulate_loop;
use crate::geometry::{Point2, Point3};
use crate::model::Id;

/// Always `Shell`: only a solid that closed into a watertight prism is ever emitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SolidForm {
    Shell,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SynthesizedSolid {
    /// One entity, or a plan entity followed by its linked vertical partners.
    pub entity_ids: Vec<Id>,
    pub provenance: ProvenanceKind,
    /// Planar closed loops, project-global mm, first vertex not repeated.
    pub faces: Vec<Vec<Point3>>,
    pub form: SolidForm,
    /// The layer of the entity whose XY footprint is authoritative (the plan entity of a pair).
    pub layer_id: String,
}

/// Least to most doubtful.
const PROVENANCE_RANK: [ProvenanceKind; 6] = [
    ProvenanceKind::Authored,
    ProvenanceKind::Proposed,
    ProvenanceKind::Defaulted,
    ProvenanceKind::Ambiguous,
    ProvenanceKind::Conflicted,
    ProvenanceKind::NotThreeD,
];

fn rank(k: ProvenanceKind) -> usize {
    PROVENANCE_RANK.iter().position(|r| *r == k).unwrap_or(0)
}

/// A solid is only as trustworthy as its worst span.
pub fn most_doubtful(kinds: &[ProvenanceKind]) -> ProvenanceKind {
    kinds.iter().fold(ProvenanceKind::Authored, |worst, &k| if rank(k) > rank(worst) { k } else { worst })
}

/// Extrude a planar loop along `axis`, offsets measured from the given loop. Bottom cap reversed
/// so both caps wind outward.
pub fn prism_from_loop(base_loop: &[Point3], axis: Point3, from: f64, to: f64) -> Vec<Vec<Point3>> {
    if base_loop.len() < 3 {
        return Vec::new();
    }
    let a = normalize3(axis);
    let bottom: Vec<Point3> = base_loop.iter().map(|&p| add3(p, scale3(a, from))).collect();
    let top: Vec<Point3> = base_loop.iter().map(|&p| add3(p, scale3(a, to))).collect();

    let n = base_loop.len();
    let mut faces: Vec<Vec<Point3>> = (0..n)
        .map(|i| {
            let j = (i + 1) % n;
            vec![bottom[i], bottom[j], top[j], top[i]]
        })
        .collect();
    faces.push(bottom.iter().rev().copied().collect());
    faces.push(top);
    faces
}

#[derive(Debug, Clone, PartialEq)]
pub struct VerticalPrism {
    pub faces: Vec<Vec<Point3>>,
    pub base_loop: Vec<Point3>,
    pub axis: Point3,
}

/// A closed shape drawn in a placed vertical view, extruded symmetrically about its cut plane along
/// the (unflipped) gaze direction.
pub fn vertical_prism(view: &ViewRef<'_>, host_view: &ViewRef<'_>, loop_local: &[Point2], depth_mm: f64) -> Result<VerticalPrism, String> {
    let basis = vertical_basis(view, host_view)?;
    let gaze = vertical_gaze_dir(basis.c_hat, false);
    let axis = Point3 { x: gaze.x, y: gaze.y, z: 0.0 };
    let base_loop = loop_local.iter().map(|&p| vertical_local_to_global3(view, host_view, p)).collect::<Result<Vec<_>, _>>()?;
    Ok(VerticalPrism { faces: prism_from_loop(&base_loop, axis, -depth_mm / 2.0, depth_mm / 2.0), base_loop, axis })
}

/// Newell's method: a planar loop's normal, robust to collinear vertices.
fn face_normal(face: &[Point3]) -> Point3 {
    let (mut nx, mut ny, mut nz) = (0.0, 0.0, 0.0);
    for (i, a) in face.iter().enumerate() {
        let b = face[(i + 1) % face.len()];
        nx += (a.y - b.y) * (a.z + b.z);
        ny += (a.z - b.z) * (a.x + b.x);
        nz += (a.x - b.x) * (a.y + b.y);
    }
    normalize3(Point3 { x: nx, y: ny, z: nz })
}

/// Triangulate every face: project onto the axis plane of its largest normal component, ear-clip in
/// 2D, lift back keeping the face's own winding.
pub fn solid_triangles(solid: &SynthesizedSolid) -> Vec<Point3> {
    let mut tris = Vec::new();
    for face in &solid.faces {
        if face.len() < 3 {
            continue;
        }
        let n = face_normal(face);
        let (ax, ay, az) = (n.x.abs(), n.y.abs(), n.z.abs());
        let flat: Vec<Point2> = if az >= ax && az >= ay {
            face.iter().map(|p| Point2 { x: p.x, y: p.y }).collect()
        } else if ax >= ay {
            face.iter().map(|p| Point2 { x: p.y, y: p.z }).collect()
        } else {
            face.iter().map(|p| Point2 { x: p.z, y: p.x }).collect()
        };

        for [i0, i1, i2] in triangulate_loop(&flat) {
            let (Some(&a), Some(&b), Some(&c)) = (face.get(i0), face.get(i1), face.get(i2)) else { continue };
            if dot3(cross3(sub3(b, a), sub3(c, a)), n) >= 0.0 {
                tris.extend([a, b, c]);
            } else {
                tris.extend([a, c, b]);
            }
        }
    }
    tris
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PrismCaps {
    pub from: f64,
    pub to: f64,
}

/// The half-space set of a prism: one plane per base-loop edge, plus both caps when given. `None`
/// caps leave the prism unbounded along its axis.
pub fn prism_half_spaces(base_loop: &[Point3], axis: Point3, caps: Option<PrismCaps>) -> Vec<HalfSpace> {
    let Some(&first) = base_loop.first() else { return Vec::new() };
    if base_loop.len() < 3 {
        return Vec::new();
    }
    let a = normalize3(axis);
    let len = base_loop.len() as f64;
    let centroid = base_loop.iter().fold(Point3 { x: 0.0, y: 0.0, z: 0.0 }, |acc, p| Point3 {
        x: acc.x + p.x / len,
        y: acc.y + p.y / len,
        z: acc.z + p.z / len,
    });

    let mut planes = Vec::new();
    for (i, &p) in base_loop.iter().enumerate() {
        let q = base_loop[(i + 1) % base_loop.len()];
        let mut n = normalize3(cross3(sub3(q, p), a));
        if n.x == 0.0 && n.y == 0.0 && n.z == 0.0 {
            continue; // edge parallel to the axis
        }
        let mut d = dot3(n, p);
        if dot3(n, centroid) > d {
            n = scale3(n, -1.0);
            d = -d;
        }
        planes.push(HalfSpace { n, d });
    }

    if let Some(caps) = caps {
        let base = dot3(a, first);
        planes.push(HalfSpace { n: a, d: base + caps.to });
        planes.push(HalfSpace { n: scale3(a, -1.0), d: -(base + caps.from) });
    }
    planes
}

/// Clip a solid by every plane in turn. `None` means the two do not overlap.
pub fn intersect_solids(a: &SynthesizedSolid, planes: &[HalfSpace]) -> Option<SynthesizedSolid> {
    let mut faces = a.faces.clone();
    for &plane in planes {
        faces = clip_convex_polyhedron(&faces, plane);
        if faces.is_empty() {
            return None;
        }
    }
    if faces.len() < 4 {
        return None;
    }
    Some(SynthesizedSolid { entity_ids: a.entity_ids.clone(), provenance: a.provenance, faces, form: SolidForm::Shell, layer_id: a.layer_id.clone() })
}
