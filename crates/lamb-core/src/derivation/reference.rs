//! Which other view a reference pane may show beside the edited one — a port of the TS app's
//! `viewport2d/referenceViews.ts` (`findEligibleReferenceViews`, `findReferenceViewId`) and
//! `ReferenceView.tsx`'s `splitOrientationFor`.

use super::read::{ProjectRead, ViewKind};
use crate::model::Id;

/// How the canvas splits for a reference pane: `Horizontal` puts the reference below the edited
/// view, `Vertical` side by side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitOrientation {
    Horizontal,
    Vertical,
}

/// Every view the reference pane may show beside `main`, in this order: for a placed vertical
/// view, its host plan (when it exists) then every other vertical placed on the same host, in
/// document order; for a plan, every vertical placed on it. An unplaced or unknown `main`, or a
/// detail, has none.
pub fn find_eligible_reference_views(p: &impl ProjectRead, main: Id) -> Vec<Id> {
    let Some(m) = p.view(main) else { return Vec::new() };
    if m.placement.is_none() {
        return Vec::new();
    }
    match m.kind {
        ViewKind::Vertical => {
            let Some(host) = m.host_view_id() else { return Vec::new() };
            let mut out: Vec<Id> = p.view(host).map(|h| h.id).into_iter().collect();
            out.extend(p.views().filter(|v| v.kind == ViewKind::Vertical && v.id != main && v.host_view_id() == Some(host)).map(|v| v.id));
            out
        }
        ViewKind::Plan => p.views().filter(|v| v.kind == ViewKind::Vertical && v.host_view_id() == Some(main)).map(|v| v.id).collect(),
        ViewKind::Detail => Vec::new(),
    }
}

/// The reference pane's default for `main`: the first eligible view (a vertical's host plan, a
/// plan's first hosted vertical).
pub fn find_reference_view_id(p: &impl ProjectRead, main: Id) -> Option<Id> {
    find_eligible_reference_views(p, main).into_iter().next()
}

/// `Vertical` (side by side) only when both views are vertical views; otherwise `Horizontal`.
pub fn split_orientation_for(p: &impl ProjectRead, main: Id, reference: Id) -> SplitOrientation {
    let vertical = |id| p.view(id).is_some_and(|v| v.kind == ViewKind::Vertical);
    if vertical(main) && vertical(reference) { SplitOrientation::Vertical } else { SplitOrientation::Horizontal }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::polyline::line2;
    use crate::geometry::{IDENTITY_TRANSFORM, Point2};
    use crate::model::{AnyView, Placement, PlanView, Project, VerticalIntent, VerticalView, empty_project};

    fn plan(id: u64, placed: bool) -> AnyView {
        let placement = placed.then_some(Placement::Plan { level_id: Id(1), cut_elevation: 1200.0, transform: IDENTITY_TRANSFORM });
        AnyView::Plan(PlanView { id: Id(id), name: format!("P{id}"), entities: Vec::new(), placement })
    }

    fn section(id: u64, host: Option<u64>) -> AnyView {
        let cut = line2(Point2 { x: 0.0, y: 0.0 }, Point2 { x: 1000.0, y: 0.0 });
        let placement = host.map(|h| Placement::Vertical { cut_line: cut, host_view_id: Id(h), transform: IDENTITY_TRANSFORM, flipped: None });
        AnyView::Vertical(VerticalView { id: Id(id), name: format!("S{id}"), intent: VerticalIntent::Section, entities: Vec::new(), placement })
    }

    fn project(views: Vec<AnyView>) -> Project {
        let mut p = empty_project(None);
        p.views = views;
        p
    }

    #[test]
    fn a_plan_lists_every_hosted_vertical_and_defaults_to_the_first() {
        let p = project(vec![plan(10, true), section(20, Some(10)), plan(11, true), section(30, Some(11)), section(21, Some(10))]);
        assert_eq!(find_eligible_reference_views(&p, Id(10)), [Id(20), Id(21)]);
        assert_eq!(find_reference_view_id(&p, Id(10)), Some(Id(20)));
        assert_eq!(find_eligible_reference_views(&p, Id(11)), [Id(30)]);
    }

    #[test]
    fn a_section_lists_its_host_then_its_sibling_sections() {
        let p = project(vec![section(20, Some(10)), plan(10, true), section(21, Some(10)), section(30, Some(11))]);
        assert_eq!(find_eligible_reference_views(&p, Id(20)), [Id(10), Id(21)]);
        assert_eq!(find_reference_view_id(&p, Id(21)), Some(Id(10)));
        // A missing host leaves just the siblings.
        assert_eq!(find_eligible_reference_views(&p, Id(30)), Vec::<Id>::new());
    }

    #[test]
    fn floating_unknown_and_unplaced_have_none() {
        let p = project(vec![plan(10, false), section(20, None), plan(11, true)]);
        assert!(find_eligible_reference_views(&p, Id(10)).is_empty());
        assert!(find_eligible_reference_views(&p, Id(20)).is_empty());
        assert!(find_eligible_reference_views(&p, Id(11)).is_empty(), "a placed plan with nothing on it");
        assert!(find_eligible_reference_views(&p, Id(99)).is_empty());
        assert_eq!(find_reference_view_id(&p, Id(10)), None);
    }

    #[test]
    fn side_by_side_only_for_two_verticals() {
        let p = project(vec![plan(10, true), section(20, Some(10)), section(21, Some(10))]);
        assert_eq!(split_orientation_for(&p, Id(20), Id(21)), SplitOrientation::Vertical);
        assert_eq!(split_orientation_for(&p, Id(10), Id(20)), SplitOrientation::Horizontal);
        assert_eq!(split_orientation_for(&p, Id(20), Id(10)), SplitOrientation::Horizontal);
    }
}
