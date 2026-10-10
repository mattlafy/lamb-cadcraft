//! Replays LAMB's golden fixtures (`tests/golden/`, copied from LAMB's `tools/fixtures/golden/`)
//! against the Rust port. The format is specified in `tests/golden/README.md`; the reference
//! implementation is LAMB's `tools/golden/codec.ts`.
//!
//! Every fixtured `"<module>#<export>"` of every package must have a line in [`dispatch`], and
//! every one of its cases must pass; a per-package progress line is printed. A fixtured export
//! with no dispatch line fails the test, which names it.
//!
//! When LAMB adds fixtures for a new export: implement it in `lamb_core`, add one line to
//! [`dispatch`], run `cargo test -p lamb-core --test golden`.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::Write as _;
use std::path::Path;

use lamb_core::derivation::{
    alignment, issues, linking, markers, projection, proposals, read, resolve, solid_cut_lines, solids, synthesis, tilt, travel, underlay,
};
use lamb_core::geometry::tangent_circle::{self, ApparentCurve};
use lamb_core::geometry::{
    Point2, Point3, Polyline2, ResolvedSegment, Transform2D, VirtualCornerEntry, circle_fit, cursor_snaps, dimension, distance, fillet, halfspace,
    hatch_pattern, intersect, loop_chain, offset, point, polyline, tolerance, transform_ops, transform2d, triangulate,
};
use lamb_core::model::{self, AnyView, DrawingEntity, DrawingLayer, HatchGeometry, Id, LineRole, LinkAnchor, LinkRecord, Project};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// Calls one port with a case's decoded positional arguments and returns its encoded result.
/// `Err(message)` means the call threw. A mutating port writes its decoded arguments back into the
/// vector (`a[i] = to_value(&x)`) so the runner can check `argsAfter`.
type Handler = fn(&mut Vec<Value>) -> Result<Value, String>;

/// The frozen clock every fixture was captured under. A port that stamps "now" must use this when
/// replaying.
const GOLDEN_NOW: &str = "2026-01-01T00:00:00.000Z";

/// |a − b| ≤ TOLERANCE · max(1, |a|, |b|).
const TOLERANCE: f64 = 1e-9;

// ---------------------------------------------------------------------------------------------
// Dispatch table
// ---------------------------------------------------------------------------------------------

fn dispatch() -> HashMap<&'static str, Handler> {
    let table: &[(&str, Handler)] = &[
        // geometry-primitives/tolerance
        ("geometry-primitives/tolerance#degToRad", |a| Ok(num_out(tolerance::deg_to_rad(num(a, 0)?)))),
        ("geometry-primitives/tolerance#radToDeg", |a| Ok(num_out(tolerance::rad_to_deg(num(a, 0)?)))),
        ("geometry-primitives/tolerance#nearlyEqual", |a| Ok(Value::Bool(tolerance::nearly_equal(num(a, 0)?, num(a, 1)?, num(a, 2)?)))),
        // geometry-primitives/point
        ("geometry-primitives/point#add2", |a| out(point::add2(arg::<Point2>(a, 0)?, arg(a, 1)?))),
        ("geometry-primitives/point#sub2", |a| out(point::sub2(arg::<Point2>(a, 0)?, arg(a, 1)?))),
        ("geometry-primitives/point#scale2", |a| out(point::scale2(arg::<Point2>(a, 0)?, num(a, 1)?))),
        ("geometry-primitives/point#lerp2", |a| out(point::lerp2(arg::<Point2>(a, 0)?, arg(a, 1)?, num(a, 2)?))),
        ("geometry-primitives/point#dist2", |a| Ok(num_out(point::dist2(arg::<Point2>(a, 0)?, arg(a, 1)?)))),
        ("geometry-primitives/point#dot2", |a| Ok(num_out(point::dot2(arg::<Point2>(a, 0)?, arg(a, 1)?)))),
        ("geometry-primitives/point#cross2", |a| Ok(num_out(point::cross2(arg::<Point2>(a, 0)?, arg(a, 1)?)))),
        ("geometry-primitives/point#normalize2", |a| out(point::normalize2(arg::<Point2>(a, 0)?))),
        ("geometry-primitives/point#angleOf2", |a| Ok(num_out(point::angle_of2(arg::<Point2>(a, 0)?)))),
        ("geometry-primitives/point#point3", |a| out(point::point3(num(a, 0)?, num(a, 1)?, num(a, 2)?))),
        ("geometry-primitives/point#add3", |a| out(point::add3(arg::<Point3>(a, 0)?, arg(a, 1)?))),
        ("geometry-primitives/point#sub3", |a| out(point::sub3(arg::<Point3>(a, 0)?, arg(a, 1)?))),
        ("geometry-primitives/point#scale3", |a| out(point::scale3(arg::<Point3>(a, 0)?, num(a, 1)?))),
        ("geometry-primitives/point#dot3", |a| Ok(num_out(point::dot3(arg::<Point3>(a, 0)?, arg(a, 1)?)))),
        ("geometry-primitives/point#cross3", |a| out(point::cross3(arg::<Point3>(a, 0)?, arg(a, 1)?))),
        ("geometry-primitives/point#normalize3", |a| out(point::normalize3(arg::<Point3>(a, 0)?))),
        ("geometry-primitives/point#dist3", |a| Ok(num_out(point::dist3(arg::<Point3>(a, 0)?, arg(a, 1)?)))),
        // geometry-primitives/transform2d
        ("geometry-primitives/transform2d#localToWorld2", |a| out(transform2d::local_to_world2(&arg::<Transform2D>(a, 0)?, arg(a, 1)?))),
        ("geometry-primitives/transform2d#worldToLocal2", |a| out(transform2d::world_to_local2(&arg::<Transform2D>(a, 0)?, arg(a, 1)?))),
        ("geometry-primitives/transform2d#localToWorldDir2", |a| out(transform2d::local_to_world_dir2(&arg::<Transform2D>(a, 0)?, arg(a, 1)?))),
        ("geometry-primitives/transform2d#worldToLocalDir2", |a| out(transform2d::world_to_local_dir2(&arg::<Transform2D>(a, 0)?, arg(a, 1)?))),
        // geometry-primitives/polyline
        ("geometry-primitives/polyline#virtualCornerSegmentIndex", |a| {
            out(polyline::virtual_corner_segment_index(&arg::<Polyline2>(a, 0)?, arg(a, 1)?))
        }),
        ("geometry-primitives/polyline#virtualCornerAt", |a| out(polyline::virtual_corner_at(&arg::<Polyline2>(a, 0)?, arg(a, 1)?))),
        ("geometry-primitives/polyline#virtualCornerEntries", |a| out(polyline::virtual_corner_entries(&arg::<Polyline2>(a, 0)?))),
        ("geometry-primitives/polyline#line2", |a| out(polyline::line2(arg::<Point2>(a, 0)?, arg(a, 1)?))),
        ("geometry-primitives/polyline#resolvedSegments", |a| out(polyline::resolved_segments(&arg::<Polyline2>(a, 0)?))),
        ("geometry-primitives/polyline#reversePolyline", |a| out(polyline::reverse_polyline(&arg::<Polyline2>(a, 0)?))),
        ("geometry-primitives/polyline#isArc", |a| {
            Ok(Value::Bool(polyline::is_arc(arg::<Value>(a, 0)?.get("bulge").and_then(Value::as_f64).unwrap_or(0.0))))
        }),
        ("geometry-primitives/polyline#arcFromBulge", |a| out(polyline::arc_from_bulge(arg::<Point2>(a, 0)?, arg(a, 1)?, num(a, 2)?))),
        ("geometry-primitives/polyline#sweepAngle", |a| Ok(num_out(polyline::sweep_angle(num(a, 0)?, num(a, 1)?, arg(a, 2)?)))),
        ("geometry-primitives/polyline#pointAtParam", |a| out(polyline::point_at_param(&arg::<ResolvedSegment>(a, 0)?, num(a, 1)?))),
        ("geometry-primitives/polyline#tangentAtParam", |a| out(polyline::tangent_at_param(&arg::<ResolvedSegment>(a, 0)?, num(a, 1)?))),
        ("geometry-primitives/polyline#segmentLength", |a| Ok(num_out(polyline::segment_length(&arg::<ResolvedSegment>(a, 0)?)))),
        ("geometry-primitives/polyline#polylineLength", |a| Ok(num_out(polyline::polyline_length(&arg::<Polyline2>(a, 0)?)))),
        ("geometry-primitives/polyline#segmentBBox", |a| out(polyline::segment_bbox(&arg::<ResolvedSegment>(a, 0)?))),
        ("geometry-primitives/polyline#polylineBBox", |a| out(polyline::polyline_bbox(&arg::<Polyline2>(a, 0)?))),
        ("geometry-primitives/polyline#closestPointOnSegment", |a| {
            out(polyline::closest_point_on_segment(&arg::<ResolvedSegment>(a, 0)?, arg(a, 1)?))
        }),
        ("geometry-primitives/polyline#slideSegmentByDelta", |a| {
            out(polyline::slide_segment_by_delta(&arg::<Polyline2>(a, 0)?, arg(a, 1)?, arg(a, 2)?))
        }),
        ("geometry-primitives/polyline#bulgeFromThroughPoint", |a| {
            Ok(num_out(polyline::bulge_from_through_point(arg::<Point2>(a, 0)?, arg(a, 1)?, arg(a, 2)?)))
        }),
        ("geometry-primitives/polyline#mergeCoArcSegments", |a| out(polyline::merge_co_arc_segments(&arg::<Polyline2>(a, 0)?))),
        ("geometry-primitives/polyline#mergeCoArcSegmentsWithMap", |a| out(polyline::merge_co_arc_segments_with_map(&arg::<Polyline2>(a, 0)?))),
        // geometry-primitives/circleFit
        ("geometry-primitives/circleFit#circleAsPolyline", |a| out(circle_fit::circle_as_polyline(&arg::<circle_fit::CircleFit>(a, 0)?))),
        ("geometry-primitives/circleFit#polylineAsCircle", |a| out(circle_fit::polyline_as_circle(&arg::<Polyline2>(a, 0)?))),
        ("geometry-primitives/circleFit#circleFromDiameter", |a| out(circle_fit::circle_from_diameter(arg::<Point2>(a, 0)?, arg(a, 1)?))),
        ("geometry-primitives/circleFit#circumcircle", |a| out(circle_fit::circumcircle(arg::<Point2>(a, 0)?, arg(a, 1)?, arg(a, 2)?))),
        // geometry-primitives/intersect
        ("geometry-primitives/intersect#infiniteLineIntersect", |a| {
            out(intersect::infinite_line_intersect(arg::<Point2>(a, 0)?, arg(a, 1)?, arg(a, 2)?, arg(a, 3)?))
        }),
        ("geometry-primitives/intersect#segmentSegmentStraight", |a| {
            out(intersect::segment_segment_straight(
                arg::<Point2>(a, 0)?,
                arg(a, 1)?,
                arg(a, 2)?,
                arg(a, 3)?,
                arg::<Option<f64>>(a, 4)?.unwrap_or(1e-6),
            ))
        }),
        ("geometry-primitives/intersect#intersectLineWithBoundarySegment", |a| {
            out(intersect::intersect_line_with_boundary_segment(
                arg::<Point2>(a, 0)?,
                arg(a, 1)?,
                arg(a, 2)?,
                arg(a, 3)?,
                arg::<Option<f64>>(a, 4)?.unwrap_or(1e-6),
            ))
        }),
        ("geometry-primitives/intersect#intersectLineWithBoundaryCircle", |a| {
            out(intersect::intersect_line_with_boundary_circle(arg::<Point2>(a, 0)?, arg(a, 1)?, arg(a, 2)?, num(a, 3)?))
        }),
        ("geometry-primitives/intersect#intersectLineWithBoundaryArc", |a| {
            out(intersect::intersect_line_with_boundary_arc(arg::<Point2>(a, 0)?, arg(a, 1)?, arg(a, 2)?, arg(a, 3)?, num(a, 4)?))
        }),
        ("geometry-primitives/intersect#intersectSegmentWithInfiniteLine", |a| {
            out(intersect::intersect_segment_with_infinite_line(
                &arg::<ResolvedSegment>(a, 0)?,
                arg(a, 1)?,
                arg(a, 2)?,
                arg::<Option<f64>>(a, 3)?.unwrap_or(1e-6),
            ))
        }),
        ("geometry-primitives/intersect#intersectSegments", |a| {
            out(intersect::intersect_segments(&arg::<ResolvedSegment>(a, 0)?, &arg(a, 1)?, arg::<Option<f64>>(a, 2)?.unwrap_or(1e-6)))
        }),
        ("geometry-primitives/intersect#crossingAngleDeg", |a| Ok(num_out(intersect::crossing_angle_deg(arg::<Point2>(a, 0)?, arg(a, 1)?)))),
        ("geometry-primitives/intersect#tangentAtHit", |a| out(intersect::tangent_at_hit(&arg::<ResolvedSegment>(a, 0)?, num(a, 1)?))),
        // geometry-primitives/distance
        ("geometry-primitives/distance#polylineDistance", |a| Ok(num_out(distance::polyline_distance(&arg::<Polyline2>(a, 0)?, &arg(a, 1)?)))),
        // geometry-primitives/fillet
        ("geometry-primitives/fillet#filletCorner", |a| out(fillet::fillet_corner(arg::<Point2>(a, 0)?, arg(a, 1)?, arg(a, 2)?, num(a, 3)?))),
        ("geometry-primitives/fillet#chamferCorner", |a| {
            out(fillet::chamfer_corner(arg::<Point2>(a, 0)?, arg(a, 1)?, arg(a, 2)?, num(a, 3)?, num(a, 4)?))
        }),
        ("geometry-primitives/fillet#resolveArcFilletCorner", |a| {
            out(fillet::resolve_arc_fillet_corner(arg::<Point2>(a, 0)?, arg(a, 1)?, arg(a, 2)?, num(a, 3)?, num(a, 4)?, num(a, 5)?))
        }),
        ("geometry-primitives/fillet#curveFromEdge", |a| out(fillet::curve_from_edge(arg::<Point2>(a, 0)?, arg(a, 1)?, num(a, 2)?))),
        ("geometry-primitives/fillet#apparentCurveIntersections", |a| {
            out(fillet::apparent_curve_intersections(&arg::<ApparentCurve>(a, 0)?, &arg(a, 1)?))
        }),
        ("geometry-primitives/fillet#paramAlongEdge", |a| {
            Ok(num_out(fillet::param_along_edge(arg::<Point2>(a, 0)?, arg(a, 1)?, num(a, 2)?, arg(a, 3)?)))
        }),
        ("geometry-primitives/fillet#chooseSharedCorner", |a| {
            out(fillet::choose_shared_corner(
                &arg::<Vec<Point2>>(a, 0)?,
                arg(a, 1)?,
                arg(a, 2)?,
                num(a, 3)?,
                arg(a, 4)?,
                arg(a, 5)?,
                arg(a, 6)?,
                num(a, 7)?,
                arg(a, 8)?,
            ))
        }),
        ("geometry-primitives/fillet#keepForParam", |a| out(fillet::keep_for_param(num(a, 0)?, num(a, 1)?))),
        ("geometry-primitives/fillet#resolveEndsKeeps", |a| out(fillet::resolve_ends_keeps(&arg::<fillet::EndsPick>(a, 0)?, &arg(a, 1)?))),
        ("geometry-primitives/fillet#bulgeForArc", |a| Ok(num_out(fillet::bulge_for_arc(arg::<Point2>(a, 0)?, arg(a, 1)?, arg(a, 2)?, arg(a, 3)?)))),
        ("geometry-primitives/fillet#resolveGapBridge", |a| {
            out(fillet::resolve_gap_bridge(&arg::<fillet::GapBridgeSide>(a, 0)?, &arg(a, 1)?, num(a, 2)?))
        }),
        ("geometry-primitives/fillet#resolveVirtualCorner", |a| {
            out(fillet::resolve_virtual_corner(&arg::<Polyline2>(a, 0)?, arg(a, 1)?, &arg::<VirtualCornerEntry>(a, 2)?))
        }),
        ("geometry-primitives/fillet#resolveVirtualPolyline", |a| out(fillet::resolve_virtual_polyline(&arg::<Polyline2>(a, 0)?))),
        // geometry-primitives/tangentCircle
        ("geometry-primitives/tangentCircle#circleCircleIntersect", |a| {
            out(tangent_circle::circle_circle_intersect(arg::<Point2>(a, 0)?, num(a, 1)?, arg(a, 2)?, num(a, 3)?))
        }),
        ("geometry-primitives/tangentCircle#offsetLine", |a| out(tangent_circle::offset_line(arg::<Point2>(a, 0)?, arg(a, 1)?, num(a, 2)?))),
        ("geometry-primitives/tangentCircle#tangentCircles", |a| {
            out(tangent_circle::tangent_circles(&arg::<ApparentCurve>(a, 0)?, &arg(a, 1)?, num(a, 2)?))
        }),
        ("geometry-primitives/tangentCircle#chooseTangentCircle", |a| {
            out(tangent_circle::choose_tangent_circle(&arg::<Vec<tangent_circle::TangentCircle>>(a, 0)?, arg(a, 1)?, arg(a, 2)?))
        }),
        // geometry-primitives/cursorSnaps
        ("geometry-primitives/cursorSnaps#nearestCandidate", |a| out(cursor_snaps::nearest_candidate(&arg::<ResolvedSegment>(a, 0)?, arg(a, 1)?))),
        ("geometry-primitives/cursorSnaps#perpendicularCandidates", |a| {
            out(cursor_snaps::perpendicular_candidates(&arg::<ResolvedSegment>(a, 0)?, arg(a, 1)?))
        }),
        ("geometry-primitives/cursorSnaps#apparentIntersectionCandidates", |a| {
            out(cursor_snaps::apparent_intersection_candidates(&arg::<ResolvedSegment>(a, 0)?, &arg(a, 1)?))
        }),
        // geometry-primitives/offset
        ("geometry-primitives/offset#offsetPolyline", |a| out(offset::offset_polyline(&arg::<Polyline2>(a, 0)?, num(a, 1)?))),
        ("geometry-primitives/offset#polylineFromPoints", |a| {
            out(offset::polyline_from_points(&arg::<Vec<Point2>>(a, 0)?, arg::<Option<bool>>(a, 1)?.unwrap_or(false)))
        }),
        ("geometry-primitives/offset#chordDistance", |a| Ok(num_out(offset::chord_distance(arg::<Point2>(a, 0)?, arg(a, 1)?)))),
        // geometry-primitives/dimension (transformDimension takes a function argument: no dispatch line)
        ("geometry-primitives/dimension#resolveDimension", |a| out(dimension::resolve_dimension(&arg::<dimension::Dimension2>(a, 0)?, num(a, 1)?))),
        // geometry-primitives/hatchPattern
        ("geometry-primitives/hatchPattern#parsePatFile", |a| out(hatch_pattern::parse_pat_file(&arg::<String>(a, 0)?))),
        ("geometry-primitives/hatchPattern#stringifyPatFile", |a| {
            out(hatch_pattern::stringify_pat_file(&arg::<Vec<hatch_pattern::PatternDefinition>>(a, 0)?))
        }),
        ("geometry-primitives/hatchPattern#findBuiltinPattern", |a| out(hatch_pattern::find_builtin_pattern(&arg::<String>(a, 0)?))),
        ("geometry-primitives/hatchPattern#dashIntervalsInRange", |a| {
            out(hatch_pattern::dash_intervals_in_range(&arg::<Vec<f64>>(a, 0)?, num(a, 1)?, num(a, 2)?, num(a, 3)?))
        }),
        // geometry-primitives/loopChain
        ("geometry-primitives/loopChain#chainBoundaryLoops", |a| {
            out(loop_chain::chain_boundary_loops(&arg::<Vec<loop_chain::LoopChainEntity>>(a, 0)?, num(a, 1)?))
        }),
        // geometry-primitives/triangulate
        ("geometry-primitives/triangulate#polygonAreaSigned", |a| Ok(num_out(triangulate::polygon_area_signed(&arg::<Vec<Point2>>(a, 0)?)))),
        ("geometry-primitives/triangulate#isConvexLoop", |a| Ok(Value::Bool(triangulate::is_convex_loop(&arg::<Vec<Point2>>(a, 0)?)))),
        ("geometry-primitives/triangulate#triangulateLoop", |a| out(triangulate::triangulate_loop(&arg::<Vec<Point2>>(a, 0)?))),
        // geometry-primitives/halfspace
        ("geometry-primitives/halfspace#sectionConvexPolyhedron", |a| {
            out(halfspace::section_convex_polyhedron(&arg::<Vec<Vec<Point3>>>(a, 0)?, arg(a, 1)?))
        }),
        ("geometry-primitives/halfspace#clipConvexPolyhedron", |a| {
            out(halfspace::clip_convex_polyhedron(&arg::<Vec<Vec<Point3>>>(a, 0)?, arg(a, 1)?))
        }),
        // geometry-primitives/transformOps
        ("geometry-primitives/transformOps#translatePoint", |a| out(transform_ops::translate_point(arg::<Point2>(a, 0)?, num(a, 1)?, num(a, 2)?))),
        ("geometry-primitives/transformOps#rotatePointAbout", |a| {
            out(transform_ops::rotate_point_about(arg::<Point2>(a, 0)?, arg(a, 1)?, num(a, 2)?))
        }),
        ("geometry-primitives/transformOps#scalePointAbout", |a| out(transform_ops::scale_point_about(arg::<Point2>(a, 0)?, arg(a, 1)?, num(a, 2)?))),
        ("geometry-primitives/transformOps#mirrorPointAcrossLine", |a| {
            out(transform_ops::mirror_point_across_line(arg::<Point2>(a, 0)?, arg(a, 1)?, arg(a, 2)?))
        }),
        // document-model/entities
        ("document-model/entities#hatchLoops", |a| out(model::hatch_loops(&arg::<HatchGeometry>(a, 0)?))),
        ("document-model/entities#isLineLike", |a| Ok(Value::Bool(model::is_line_like(&arg::<DrawingEntity>(a, 0)?)))),
        // document-model/layers
        ("document-model/layers#defaultLayers", |_| out(model::default_layers())),
        ("document-model/layers#isLayerRendered", |a| Ok(Value::Bool(model::is_layer_rendered(&arg::<DrawingLayer>(a, 0)?)))),
        ("document-model/layers#isLayerSnappable", |a| Ok(Value::Bool(model::is_layer_snappable(&arg::<DrawingLayer>(a, 0)?)))),
        ("document-model/layers#isLayerSelectable", |a| Ok(Value::Bool(model::is_layer_selectable(&arg::<DrawingLayer>(a, 0)?)))),
        ("document-model/layers#isLayerEditable", |a| Ok(Value::Bool(model::is_layer_editable(&arg::<DrawingLayer>(a, 0)?)))),
        ("document-model/layers#resolveEntityRole", |a| {
            out(model::resolve_entity_role(&arg::<Vec<DrawingLayer>>(a, 0)?, arg::<LineRole>(a, 1)?, &arg::<String>(a, 2)?))
        }),
        // document-model/provenance
        ("document-model/provenance#authoredField", |a| {
            out(model::authored_field(arg::<Value>(a, 0)?, arg::<Id>(a, 1)?, arg(a, 2)?, &arg::<String>(a, 3)?))
        }),
        ("document-model/provenance#defaultedField", |a| out(model::defaulted_field(arg::<Value>(a, 0)?, &arg::<String>(a, 1)?))),
        // document-model/views
        ("document-model/views#isPlanView", |a| Ok(Value::Bool(model::is_plan_view(&arg::<AnyView>(a, 0)?)))),
        ("document-model/views#isVerticalView", |a| Ok(Value::Bool(model::is_vertical_view(&arg::<AnyView>(a, 0)?)))),
        ("document-model/views#isPlaced", |a| Ok(Value::Bool(model::is_placed(&arg::<AnyView>(a, 0)?)))),
        // document-model/project
        ("document-model/project#defaultProjectDefaults", |_| out(model::default_project_defaults())),
        ("document-model/project#emptyProject", |a| out(model::empty_project(arg::<Option<String>>(a, 0)?.as_deref()))),
        ("document-model/project#findView", |a| out(model::find_view(&arg::<Project>(a, 0)?, arg(a, 1)?))),
        ("document-model/project#findLayer", |a| out(model::find_layer(&arg::<Project>(a, 0)?, &arg::<String>(a, 1)?))),
        ("document-model/project#findLevel", |a| out(model::find_level(&arg::<Project>(a, 0)?, arg(a, 1)?))),
        // derivation-engine/projection
        ("derivation-engine/projection#planLocalToGlobalXY", |a| {
            out(projection::plan_local_to_global_xy(&read::view_ref(&arg::<AnyView>(a, 0)?), arg(a, 1)?)?)
        }),
        ("derivation-engine/projection#verticalGazeDir", |a| {
            #[derive(serde::Deserialize)]
            struct GazeArg {
                #[serde(rename = "cHat")]
                c_hat: Point2,
                #[serde(default)]
                flipped: Option<bool>,
            }
            let g = arg::<GazeArg>(a, 0)?;
            out(projection::vertical_gaze_dir(g.c_hat, g.flipped.unwrap_or(false)))
        }),
        ("derivation-engine/projection#planPlacementThrough", |a| {
            out(projection::plan_placement_through(&read::view_ref(&arg::<AnyView>(a, 0)?), &read::view_ref(&arg::<AnyView>(a, 1)?), arg(a, 2)?)?)
        }),
        ("derivation-engine/projection#perpendicularCutLineThrough", |a| {
            out(projection::perpendicular_cut_line_through(
                &read::view_ref(&arg::<AnyView>(a, 0)?),
                &read::view_ref(&arg::<AnyView>(a, 1)?),
                arg(a, 2)?,
                num(a, 3)?,
            )?)
        }),
        // derivation-engine/alignment
        ("derivation-engine/alignment#computeAlignmentBasis", |a| {
            out(alignment::compute_alignment_basis(&read::view_ref(&arg::<AnyView>(a, 0)?), &read::view_ref(&arg::<AnyView>(a, 1)?))?)
        }),
        ("derivation-engine/alignment#localDirectionAlongCHat", |a| {
            Ok(num_out(alignment::local_direction_along_c_hat(&read::view_ref(&arg::<AnyView>(a, 0)?), arg(a, 1)?)))
        }),
        ("derivation-engine/alignment#planPointToUV", |a| {
            out(alignment::plan_point_to_uv(&read::view_ref(&arg::<AnyView>(a, 0)?), &arg(a, 1)?, arg(a, 2)?)?)
        }),
        ("derivation-engine/alignment#uvToPlanPoint", |a| {
            out(alignment::uv_to_plan_point(&read::view_ref(&arg::<AnyView>(a, 0)?), &arg(a, 1)?, arg(a, 2)?)?)
        }),
        // derivation-engine/tilt, underlay, travel, resolve
        ("derivation-engine/tilt#tiltFromCrossing", |a| Ok(num_out(tilt::tilt_from_crossing(num(a, 0)?, num(a, 1)?)))),
        ("derivation-engine/underlay#findUnderlayCandidates", |a| {
            let p = arg::<Project>(a, 0)?;
            let found = underlay::find_underlay_candidates(&p, arg(a, 1)?)?;
            let views: Vec<Value> = found
                .iter()
                .map(|c| Ok(serde_json::json!({ "view": out(model::find_view(&p, c.view_id).ok_or("candidate view not found")?)?, "mirrored": c.mirrored })))
                .collect::<Result<_, String>>()?;
            Ok(Value::Array(views))
        }),
        ("derivation-engine/underlay#mapViewLocalPoint", |a| {
            let (from, to) = (arg::<AnyView>(a, 1)?, arg::<AnyView>(a, 2)?);
            out(underlay::map_view_local_point(&arg::<Project>(a, 0)?, &read::view_ref(&from), &read::view_ref(&to), arg(a, 3)?)?)
        }),
        ("derivation-engine/underlay#mapPlanVerticalCenter", |a| {
            let (from, to) = (arg::<AnyView>(a, 1)?, arg::<AnyView>(a, 3)?);
            out(underlay::map_plan_vertical_center(&arg::<Project>(a, 0)?, &read::view_ref(&from), arg(a, 2)?, &read::view_ref(&to), arg(a, 4)?)?)
        }),
        ("derivation-engine/travel#travelSegmentMove", |a| {
            out(travel::travel_segment_move(&arg::<Project>(a, 0)?, &arg::<LinkRecord>(a, 1)?, &arg::<LinkAnchor>(a, 2)?, arg(a, 3)?)?)
        }),
        ("derivation-engine/resolve#resolveSeenZ", |a| {
            out(resolve::resolve_seen_z(&arg::<Project>(a, 0)?, &read::entity_ref(&arg::<DrawingEntity>(a, 1)?), arg(a, 2)?)?)
        }),
        ("derivation-engine/resolve#resolveCutSegment", |a| {
            out(resolve::resolve_cut_segment(&arg::<Project>(a, 0)?, &read::entity_ref(&arg::<DrawingEntity>(a, 1)?), arg(a, 2)?)?)
        }),
        // derivation-engine/solids, synthesis
        ("derivation-engine/solids#mostDoubtful", |a| out(solids::most_doubtful(&arg::<Vec<resolve::ProvenanceKind>>(a, 0)?))),
        ("derivation-engine/solids#prismFromLoop", |a| out(solids::prism_from_loop(&arg::<Vec<Point3>>(a, 0)?, arg(a, 1)?, num(a, 2)?, num(a, 3)?))),
        ("derivation-engine/solids#solidTriangles", |a| out(solids::solid_triangles(&arg::<solids::SynthesizedSolid>(a, 0)?))),
        ("derivation-engine/synthesis#synthesize", |a| out(synthesis::synthesize(&arg::<Project>(a, 0)?)?)),
        // derivation-engine/markers, linking
        ("derivation-engine/markers#computeMarkersForVerticalView", |a| {
            out(markers::compute_markers_for_vertical_view(&arg::<Project>(a, 0)?, &read::view_ref(&arg::<AnyView>(a, 1)?))?)
        }),
        ("derivation-engine/markers#computeMarkersForPlanView", |a| {
            out(markers::compute_markers_for_plan_view(&arg::<Project>(a, 0)?, &read::view_ref(&arg::<AnyView>(a, 1)?))?)
        }),
        ("derivation-engine/markers#computeCutLinesForView", |a| out(markers::compute_cut_lines_for_view(&arg::<Project>(a, 0)?, arg(a, 1)?)?)),
        ("derivation-engine/markers#computePlanCutLinesForView", |a| {
            out(markers::compute_plan_cut_lines_for_view(&arg::<Project>(a, 0)?, arg(a, 1)?))
        }),
        ("derivation-engine/markers#computeSolidCutsForView", |a| {
            let solids = arg::<Option<Vec<solids::SynthesizedSolid>>>(a, 2)?;
            out(markers::compute_solid_cuts_for_view(&arg::<Project>(a, 0)?, arg(a, 1)?, solids.as_deref())?)
        }),
        ("derivation-engine/linking#findAutoLinks", |a| {
            let mut n = 0;
            let mut mint = || {
                n += 1;
                Id(n)
            };
            let entity = arg::<DrawingEntity>(a, 1)?;
            out(linking::find_auto_links(&arg::<Project>(a, 0)?, &read::entity_ref(&entity), &arg(a, 2)?, &arg::<String>(a, 3)?, &mut mint)?)
        }),
        ("derivation-engine/linking#findLinkForMarker", |a| {
            out(linking::find_link_for_marker(&arg::<Project>(a, 0)?, &arg(a, 1)?, arg(a, 2)?).cloned())
        }),
        // derivation-engine/proposals
        ("derivation-engine/proposals#aiProposalSlotKey", |a| out(proposals::ai_proposal_slot_key(arg(a, 0)?, arg(a, 1)?, arg(a, 2)?))),
        ("derivation-engine/proposals#reconcileAiLines", |a| {
            let mut project = arg::<Project>(a, 0)?;
            let mut n = 0;
            let mut mint = || {
                n += 1;
                Id(n)
            };
            let result = proposals::reconcile_ai_lines(&mut project, GOLDEN_NOW, &mut mint)?;
            a[0] = out(&project)?;
            out(result)
        }),
        ("derivation-engine/proposals#invertReconcileAiLines", |a| {
            let mut project = arg::<Project>(a, 0)?;
            proposals::invert_reconcile_ai_lines(&mut project, &arg::<proposals::ReconcileResult>(a, 1)?);
            a[0] = out(&project)?;
            Ok(serde_json::json!({"$undefined": true}))
        }),
        // derivation-engine/solidCutLines, issues
        ("derivation-engine/solidCutLines#solidCutSlotKey", |a| out(solid_cut_lines::solid_cut_slot_key(&arg::<Vec<Id>>(a, 0)?, arg(a, 1)?))),
        ("derivation-engine/solidCutLines#reconcileSolidCutLines", |a| {
            let mut project = arg::<Project>(a, 0)?;
            let mut n = 0;
            let mut mint = || {
                n += 1;
                Id(n)
            };
            let result = solid_cut_lines::reconcile_solid_cut_lines(&mut project, &mut mint)?;
            a[0] = out(&project)?;
            out(result)
        }),
        ("derivation-engine/issues#findIssues", |a| out(issues::find_issues(&arg::<Project>(a, 0)?)?)),
    ];
    table.iter().copied().collect()
}

// ---------------------------------------------------------------------------------------------
// Argument / result helpers for handlers
// ---------------------------------------------------------------------------------------------

/// Positional argument `i`, deserialized. A missing trailing argument is `null` (JS `undefined`),
/// so `Option<T>` parameters work.
fn arg<T: DeserializeOwned>(args: &[Value], i: usize) -> Result<T, String> {
    let v = args.get(i).cloned().unwrap_or(Value::Null);
    serde_json::from_value(v).map_err(|e| format!("arg {i}: {e}"))
}

/// A numeric argument, accepting the `{"$number": "NaN" | "Infinity" | "-Infinity"}` tag.
fn num(args: &[Value], i: usize) -> Result<f64, String> {
    match args.get(i) {
        Some(Value::Number(n)) => n.as_f64().ok_or_else(|| format!("arg {i}: bad number")),
        Some(Value::Object(o)) => match o.get("$number").and_then(Value::as_str) {
            Some("NaN") => Ok(f64::NAN),
            Some("Infinity") => Ok(f64::INFINITY),
            Some("-Infinity") => Ok(f64::NEG_INFINITY),
            _ => Err(format!("arg {i}: not a number: {}", Value::Object(o.clone()))),
        },
        other => Err(format!("arg {i}: not a number: {other:?}")),
    }
}

/// Encode a number result, tagging non-finite values as the format requires.
fn num_out(x: f64) -> Value {
    if x.is_nan() {
        tagged_number("NaN")
    } else if x == f64::INFINITY {
        tagged_number("Infinity")
    } else if x == f64::NEG_INFINITY {
        tagged_number("-Infinity")
    } else {
        serde_json::Number::from_f64(x).map_or(Value::Null, Value::Number)
    }
}

fn tagged_number(s: &str) -> Value {
    let mut m = serde_json::Map::new();
    m.insert("$number".into(), Value::String(s.into()));
    Value::Object(m)
}

/// Encode a result through serde. NOTE: serde_json writes a non-finite f64 inside a struct as
/// `null`, so a struct result holding NaN/Infinity will show as a mismatch (never a false pass).
fn out<T: Serialize>(v: T) -> Result<Value, String> {
    serde_json::to_value(v).map_err(|e| format!("encode result: {e}"))
}

// ---------------------------------------------------------------------------------------------
// Codec: decode, minted-id canonicalisation, comparison (mirrors tools/golden/codec.ts)
// ---------------------------------------------------------------------------------------------

/// Fixture JSON → the plain JSON handed to handlers: `$undefined` → `null`, `$map` → array of
/// `[key, value]` pairs, `$set` → array. `$number` stays tagged (serde_json can't hold NaN);
/// [`num`] reads it.
fn decode(v: &Value) -> Value {
    match v {
        Value::Array(a) => Value::Array(a.iter().map(decode).collect()),
        Value::Object(o) => {
            if o.get("$undefined") == Some(&Value::Bool(true)) {
                return Value::Null;
            }
            if o.get("$number").is_some_and(Value::is_string) {
                return v.clone();
            }
            if let Some(Value::Array(pairs)) = o.get("$map") {
                return Value::Array(pairs.iter().map(decode).collect());
            }
            if let Some(Value::Array(items)) = o.get("$set") {
                return Value::Array(items.iter().map(decode).collect());
            }
            Value::Object(o.iter().map(|(k, v)| (k.clone(), decode(v))).collect())
        }
        _ => v.clone(),
    }
}

fn collect_strings<'a>(v: &'a Value, into: &mut HashSet<&'a str>) {
    match v {
        Value::String(s) => {
            into.insert(s);
        }
        Value::Array(a) => a.iter().for_each(|x| collect_strings(x, into)),
        Value::Object(o) => o.values().for_each(|x| collect_strings(x, into)),
        _ => {}
    }
}

/// `generateId`'s shape: `^[a-z][a-z0-9-]*-[0-9a-z]{8,}-[0-9a-z]+$`.
fn is_minted_id(s: &str) -> bool {
    let alnum = |p: &str| p.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    let mut parts = s.rsplitn(3, '-');
    let (Some(last), Some(mid), Some(prefix)) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    !last.is_empty()
        && alnum(last)
        && mid.len() >= 8
        && alnum(mid)
        && prefix.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && prefix.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Replace minted ids (id-shaped strings absent from the args) by `$id:N` in order of first
/// appearance, walking `values` in order with object keys sorted.
fn canonicalize(values: &[Value], given: &HashSet<&str>) -> Vec<Value> {
    fn walk(v: &Value, given: &HashSet<&str>, ids: &mut HashMap<String, String>) -> Value {
        match v {
            Value::String(s) => {
                if given.contains(s.as_str()) || !is_minted_id(s) {
                    return v.clone();
                }
                let n = ids.len();
                Value::String(ids.entry(s.clone()).or_insert_with(|| format!("$id:{n}")).clone())
            }
            Value::Array(a) => Value::Array(a.iter().map(|x| walk(x, given, ids)).collect()),
            Value::Object(o) => {
                let sorted: BTreeMap<&String, &Value> = o.iter().collect();
                Value::Object(sorted.into_iter().map(|(k, x)| (k.clone(), walk(x, given, ids))).collect())
            }
            _ => v.clone(),
        }
    }
    let mut ids = HashMap::new();
    values.iter().map(|v| walk(v, given, &mut ids)).collect()
}

/// Differences between expected and actual, as `path: expected vs actual` lines.
fn compare(expected: &Value, actual: &Value, path: &str, out: &mut Vec<String>) {
    match (expected, actual) {
        (Value::Number(e), Value::Number(a)) => {
            let (e, a) = (e.as_f64().unwrap_or(f64::NAN), a.as_f64().unwrap_or(f64::NAN));
            // Written so NaN on either side is a difference.
            let within = (e - a).abs() <= TOLERANCE * 1f64.max(e.abs()).max(a.abs());
            if !within {
                out.push(format!("{path}: {e} vs {a}"));
            }
        }
        (Value::Array(e), Value::Array(a)) => {
            if e.len() != a.len() {
                out.push(format!("{path}: length {} vs {}", e.len(), a.len()));
            } else {
                for (i, (x, y)) in e.iter().zip(a).enumerate() {
                    compare(x, y, &format!("{path}[{i}]"), out);
                }
            }
        }
        (Value::Object(e), Value::Object(a)) => {
            let keys: BTreeSet<&String> = e.keys().chain(a.keys()).collect();
            for k in keys {
                compare(e.get(k).unwrap_or(&Value::Null), a.get(k).unwrap_or(&Value::Null), &format!("{path}.{k}"), out);
            }
        }
        _ => {
            if expected != actual {
                out.push(format!("{path}: {} vs {}", short(expected), short(actual)));
            }
        }
    }
}

fn short(v: &Value) -> String {
    let s = v.to_string();
    if s.chars().count() > 80 { format!("{}...", s.chars().take(77).collect::<String>()) } else { s }
}

// ---------------------------------------------------------------------------------------------
// Runner
// ---------------------------------------------------------------------------------------------

/// Replays one case through its handler. `Ok(())` = pass, `Err(lines)` = what differed.
fn replay(case: &Value, handler: Handler) -> Result<(), Vec<String>> {
    let raw_args = case.get("args").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut args: Vec<Value> = raw_args.iter().map(decode).collect();
    let args_before = args.clone();
    // One string-id interner per case, covering argument decoding and result encoding.
    let outcome = model::with_string_ids(|| handler(&mut args));
    match (case.get("throws"), outcome) {
        (Some(_), Err(_)) => Ok(()),
        (Some(msg), Ok(v)) => Err(vec![format!("expected throw {msg}, got {}", short(&v))]),
        (None, Err(e)) => Err(vec![format!("threw: {e}")]),
        (None, Ok(v)) => {
            let given = {
                let mut s = HashSet::new();
                raw_args.iter().for_each(|a| collect_strings(a, &mut s));
                s
            };
            // Result and arguments-afterwards canonicalize together, so a minted id numbers the same
            // in both.
            let canon = canonicalize(&[v, Value::Array(args)], &given);
            let expected = case.get("result").unwrap_or(&Value::Null);
            let mut diffs = Vec::new();
            compare(expected, &canon[0], "$", &mut diffs);
            // No argsAfter recorded means the arguments must be unchanged (fixture README).
            match case.get("argsAfter") {
                Some(after) => compare(after, &canon[1], "argsAfter", &mut diffs),
                None => compare(&Value::Array(args_before), &canon[1], "args", &mut diffs),
            }
            if diffs.is_empty() { Ok(()) } else { Err(diffs) }
        }
    }
}

#[derive(Default)]
struct ExportStats {
    cases: usize,
    passed: usize,
}

#[test]
fn golden_fixtures() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("golden");
    let table = dispatch();
    // "<module>#<export>" → stats, across every fixture file (a module's cases can come from other
    // packages' test files).
    let mut stats: BTreeMap<String, ExportStats> = BTreeMap::new();
    let mut failures: Vec<String> = Vec::new();

    let mut dirs: Vec<_> = std::fs::read_dir(&root).expect("read tests/golden").filter_map(Result::ok).map(|e| e.path()).collect();
    dirs.sort();
    for dir in dirs.iter().filter(|d| d.is_dir()) {
        let mut files: Vec<_> = std::fs::read_dir(dir).expect("read package dir").filter_map(Result::ok).map(|e| e.path()).collect();
        files.sort();
        for file in files.iter().filter(|f| f.extension().is_some_and(|x| x == "json")) {
            let text = std::fs::read_to_string(file).expect("read fixture");
            let doc: Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
            assert_eq!(doc.get("format").and_then(Value::as_str), Some("lamb-golden/1"), "{}", file.display());
            for case in doc.get("cases").and_then(Value::as_array).into_iter().flatten() {
                let module = case.get("module").and_then(Value::as_str).unwrap_or("?");
                let export = case.get("export").and_then(Value::as_str).unwrap_or("?");
                let key = format!("{module}#{export}");
                let entry = stats.entry(key.clone()).or_default();
                entry.cases += 1;
                let Some(handler) = table.get(key.as_str()) else { continue };
                match replay(case, *handler) {
                    Ok(()) => entry.passed += 1,
                    Err(diffs) => {
                        let test = case.get("test").and_then(Value::as_str).unwrap_or("?");
                        let rel = file.strip_prefix(&root).unwrap_or(file).display().to_string();
                        failures.push(format!("FAIL {key} [{rel}] {test}\n    {}", diffs.into_iter().take(8).collect::<Vec<_>>().join("\n    ")));
                    }
                }
            }
        }
    }

    // Report. Written straight to the stderr handle so libtest's capture doesn't hide it.
    let mut report = String::new();
    let mut packages: BTreeMap<&str, (usize, usize, usize, usize)> = BTreeMap::new();
    for (key, s) in &stats {
        let package = key.split('/').next().unwrap_or("?");
        let p = packages.entry(package).or_default();
        p.1 += 1;
        p.3 += s.cases;
        if table.contains_key(key.as_str()) {
            p.0 += 1;
            p.2 += s.passed;
            let mark = if s.passed == s.cases { "ok  " } else { "FAIL" };
            report.push_str(&format!("  {mark} {key}: {}/{} cases pass\n", s.passed, s.cases));
        }
    }
    for (package, (ported, exports, passed, cases)) in &packages {
        report.push_str(&format!("{package}: ported {ported}/{exports} exports, {passed}/{cases} cases pass\n"));
    }
    let unfixtured: Vec<&str> = table.keys().copied().filter(|k| !stats.contains_key(*k)).collect();
    if !unfixtured.is_empty() {
        let mut u = unfixtured;
        u.sort_unstable();
        report.push_str(&format!("ported with no fixture cases: {}\n", u.join(", ")));
    }
    for f in &failures {
        report.push_str(f);
        report.push('\n');
    }
    let _ = std::io::stderr().write_all(format!("\ngolden fixtures:\n{report}").as_bytes());

    assert!(failures.is_empty(), "{} golden case(s) failed for ported exports (see report above)", failures.len());

    // Every export of every package with fixture cases must be ported.
    let unported: Vec<&str> = stats.keys().map(String::as_str).filter(|k| !table.contains_key(k)).collect();
    assert!(unported.is_empty(), "exports with fixture cases but no dispatch line: {}", unported.join(", "));
}

// ---------------------------------------------------------------------------------------------
// Codec self-checks
// ---------------------------------------------------------------------------------------------

#[test]
fn minted_id_shape() {
    assert!(is_minted_id("view-lx3k9a2b-1"));
    assert!(is_minted_id("ref-line-lx3k9a2b-1a"));
    assert!(!is_minted_id("view-short-1"));
    assert!(!is_minted_id("1view-lx3k9a2b-1"));
    assert!(!is_minted_id("lx3k9a2b-1"));
    assert!(!is_minted_id("view-lx3k9a2b-"));
    // The shape lamb-core gives an id the string-id interner never saw.
    assert!(is_minted_id("lamb-00000007-0"));
    assert!(is_minted_id(&format!("lamb-{:08x}-0", u64::MAX)));
}

#[test]
fn canonicalize_numbers_minted_ids_by_first_appearance() {
    let given: HashSet<&str> = ["keep-abcdefgh-1"].into_iter().collect();
    let v: Value = serde_json::json!({"b": "new-abcdefgh-2", "a": ["keep-abcdefgh-1", "new-zzzzzzzz-3", "new-abcdefgh-2"]});
    let c = canonicalize(&[v], &given);
    assert_eq!(c[0], serde_json::json!({"a": ["keep-abcdefgh-1", "$id:0", "$id:1"], "b": "$id:1"}));
}

#[test]
fn compare_tolerance_and_absent_key() {
    let mut d = Vec::new();
    compare(&serde_json::json!({"x": 1.0, "y": null}), &serde_json::json!({"x": 1.0 + 5e-10}), "$", &mut d);
    assert!(d.is_empty(), "{d:?}");
    compare(&serde_json::json!(1.0), &serde_json::json!(1.000_001), "$", &mut d);
    assert_eq!(d.len(), 1);
}
