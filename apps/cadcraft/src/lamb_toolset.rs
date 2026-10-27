//! The LAMB tab of the Tool Sets palette (fork, LAMB T465): its groups, tools and icons, handed to
//! ui-egui's Tool Sets slot. Icons are 24×24-grid strokes; accent strokes mark the action part.
//! Motifs: a square is a plan, a bold horizontal line a cut, a circle a detail bubble, two
//! rings a link, a folder or page the project.

use cadcraft_ui_egui::icons::{IconDef, IconPath, IconPath::*, IconStroke};
use cadcraft_ui_egui::palettes::{ExtGroup, ExtTab, ExtTool};

const fn b(path: IconPath) -> IconStroke {
    IconStroke { path, accent: false }
}
const fn a(path: IconPath) -> IconStroke {
    IconStroke { path, accent: true }
}
const fn tool(icon: IconDef, command: &'static str, tip: &'static str) -> ExtTool {
    ExtTool { icon, command, tip }
}

// Views.
const NEW_PLAN: IconDef =
    &[b(Loop(&[(3.0, 6.0), (17.0, 6.0), (17.0, 21.0), (3.0, 21.0)])), a(Line(&[(19.0, 2.0), (19.0, 9.0)])), a(Line(&[(15.5, 5.5), (22.5, 5.5)]))];
const NEW_SECTION_LINE: IconDef = &[
    b(Loop(&[(5.0, 5.0), (19.0, 5.0), (19.0, 19.0), (5.0, 19.0)])),
    a(Line(&[(2.0, 12.0), (22.0, 12.0)])),
    a(Line(&[(2.0, 12.0), (2.0, 17.0)])),
    a(Line(&[(22.0, 12.0), (22.0, 17.0)])),
];
const PLAN_HERE: IconDef = &[b(Loop(&[(4.0, 21.0), (4.0, 10.0), (12.0, 3.0), (20.0, 10.0), (20.0, 21.0)])), a(Line(&[(1.0, 14.0), (23.0, 14.0)]))];
const EDIT_VIEW: IconDef =
    &[b(Loop(&[(2.0, 4.0), (16.0, 4.0), (16.0, 18.0), (2.0, 18.0)])), a(Loop(&[(9.0, 21.0), (9.0, 17.0), (18.0, 8.0), (22.0, 12.0), (13.0, 21.0)]))];
const NEW_OFFSET_PLAN: IconDef = &[
    b(Loop(&[(2.0, 15.0), (15.0, 15.0), (22.0, 20.0), (9.0, 20.0)])),
    a(Loop(&[(2.0, 4.0), (15.0, 4.0), (22.0, 9.0), (9.0, 9.0)])),
    a(Line(&[(12.0, 9.0), (12.0, 15.0)])),
];
const NEW_ELEVATION_LINE: IconDef = &[
    b(Loop(&[(5.0, 3.0), (19.0, 3.0), (19.0, 14.0), (5.0, 14.0)])),
    a(Line(&[(3.0, 21.0), (21.0, 21.0)])),
    a(Line(&[(8.0, 21.0), (12.0, 17.0), (16.0, 21.0)])),
];
const NEW_DETAIL: IconDef = &[b(Loop(&[(2.0, 2.0), (16.0, 2.0), (16.0, 16.0), (2.0, 16.0)])), a(Circle(15.0, 15.0, 6.5))];
const DUPLICATE_VIEW: IconDef =
    &[b(Loop(&[(2.0, 2.0), (14.0, 2.0), (14.0, 14.0), (2.0, 14.0)])), a(Loop(&[(9.0, 9.0), (22.0, 9.0), (22.0, 22.0), (9.0, 22.0)]))];
const UNPLACE_VIEW: IconDef =
    &[b(Loop(&[(2.0, 2.0), (16.0, 2.0), (16.0, 16.0), (2.0, 16.0)])), a(Line(&[(14.0, 14.0), (22.0, 22.0)])), a(Line(&[(22.0, 14.0), (14.0, 22.0)]))];
const VIEW_PROPS: IconDef = &[
    b(Loop(&[(3.0, 3.0), (21.0, 3.0), (21.0, 21.0), (3.0, 21.0)])),
    a(Line(&[(7.0, 8.0), (17.0, 8.0)])),
    a(Line(&[(7.0, 12.0), (17.0, 12.0)])),
    a(Line(&[(7.0, 16.0), (13.0, 16.0)])),
];
const LEVEL: IconDef = &[
    b(Line(&[(10.0, 8.0), (22.0, 8.0)])),
    b(Line(&[(10.0, 19.0), (22.0, 19.0)])),
    a(Loop(&[(6.0, 8.0), (3.0, 3.0), (9.0, 3.0)])),
    a(Loop(&[(6.0, 19.0), (3.0, 14.0), (9.0, 14.0)])),
];
const CUT_LINE: IconDef = &[
    a(Line(&[(2.0, 14.0), (22.0, 14.0)])),
    b(Line(&[(5.0, 14.0), (5.0, 5.0)])),
    b(Line(&[(3.0, 7.0), (5.0, 5.0), (7.0, 7.0)])),
    b(Line(&[(19.0, 14.0), (19.0, 5.0)])),
    b(Line(&[(17.0, 7.0), (19.0, 5.0), (21.0, 7.0)])),
];

// Links.
const LINK: IconDef = &[b(Circle(9.0, 12.0, 5.5)), a(Circle(15.0, 12.0, 5.5))];
const UNLINK: IconDef = &[b(Circle(6.5, 12.0, 4.5)), b(Circle(17.5, 12.0, 4.5)), a(Line(&[(10.0, 21.0), (14.0, 3.0)]))];
const RESOLVE: IconDef = &[b(Circle(6.0, 9.0, 4.0)), b(Circle(12.0, 9.0, 4.0)), a(Line(&[(11.0, 17.0), (14.5, 21.0), (22.0, 12.0)]))];
const ACCEPT: IconDef = &[
    b(Line(&[(2.0, 18.0), (5.0, 15.0)])),
    b(Line(&[(7.0, 13.0), (10.0, 10.0)])),
    b(Line(&[(12.0, 8.0), (15.0, 5.0)])),
    a(Line(&[(11.0, 17.0), (14.5, 21.0), (22.0, 11.0)])),
];
const SET_ROLE: IconDef = &[
    b(Line(&[(3.0, 20.0), (19.0, 4.0)])),
    a(Loop(&[(12.0, 13.0), (18.0, 13.0), (22.0, 17.0), (18.0, 21.0), (12.0, 21.0)])),
    a(Dot(14.5, 17.0, 1.2)),
];
const LAYER_ROLE: IconDef = &[
    b(Loop(&[(2.0, 7.0), (11.0, 3.0), (20.0, 7.0), (11.0, 11.0)])),
    b(Line(&[(2.0, 11.0), (11.0, 15.0), (20.0, 11.0)])),
    a(Loop(&[(13.0, 16.0), (18.0, 16.0), (22.0, 19.0), (18.0, 22.0), (13.0, 22.0)])),
];

// Panes.
const REF_PANE: IconDef = &[
    b(Loop(&[(2.0, 4.0), (22.0, 4.0), (22.0, 20.0), (2.0, 20.0)])),
    b(Line(&[(12.0, 4.0), (12.0, 20.0)])),
    a(Loop(&[(15.0, 9.0), (19.0, 9.0), (19.0, 15.0), (15.0, 15.0)])),
];
const UNDERLAY: IconDef = &[
    b(Loop(&[(4.0, 4.0), (20.0, 4.0), (20.0, 20.0), (4.0, 20.0)])),
    a(Line(&[(4.0, 14.0), (14.0, 4.0)])),
    a(Line(&[(4.0, 20.0), (20.0, 4.0)])),
    a(Line(&[(10.0, 20.0), (20.0, 10.0)])),
];
const PANE_LOCK: IconDef = &[
    b(Loop(&[(2.0, 3.0), (22.0, 3.0), (22.0, 21.0), (2.0, 21.0)])),
    b(Line(&[(12.0, 3.0), (12.0, 8.0)])),
    a(Loop(&[(8.5, 12.0), (15.5, 12.0), (15.5, 18.0), (8.5, 18.0)])),
    a(Arc(12.0, 12.0, 2.5, 0.0, 180.0)),
];
const UCS_ALIGN: IconDef =
    &[b(Line(&[(3.0, 3.0), (3.0, 21.0), (21.0, 21.0)])), a(Line(&[(5.0, 6.0), (10.0, 19.0), (22.0, 14.0)])), a(Dot(10.0, 19.0, 1.4))];

// Details.
const DETAIL_TAG: IconDef = &[b(Circle(15.0, 9.0, 6.5)), b(Line(&[(8.5, 9.0), (21.5, 9.0)])), a(Line(&[(10.5, 14.0), (3.0, 21.0)]))];
const DETAIL_REF: IconDef = &[
    b(Line(&[(5.0, 2.0), (5.0, 22.0)])),
    b(Line(&[(9.0, 2.0), (9.0, 22.0)])),
    a(Line(&[(22.0, 12.0), (10.5, 12.0)])),
    a(Line(&[(14.0, 9.0), (10.5, 12.0), (14.0, 15.0)])),
];
const REPEAT: IconDef = &[
    b(Loop(&[(3.0, 4.0), (6.0, 4.0), (6.0, 17.0), (3.0, 17.0)])),
    a(Loop(&[(10.5, 4.0), (13.5, 4.0), (13.5, 17.0), (10.5, 17.0)])),
    a(Loop(&[(18.0, 4.0), (21.0, 4.0), (21.0, 17.0), (18.0, 17.0)])),
    b(Line(&[(4.5, 21.0), (19.5, 21.0)])),
];
const OUTSIDE_POINT: IconDef =
    &[b(Loop(&[(3.0, 3.0), (15.0, 3.0), (15.0, 15.0), (3.0, 15.0)])), a(Line(&[(20.0, 20.0), (16.5, 16.5)])), a(Dot(20.0, 20.0, 2.0))];
const DETAIL_TAGS: IconDef =
    &[a(Circle(7.0, 7.0, 4.0)), b(Line(&[(13.0, 7.0), (21.0, 7.0)])), a(Circle(7.0, 17.0, 4.0)), b(Line(&[(13.0, 17.0), (21.0, 17.0)]))];

// Assembly.
const ASSEMBLY_PLAN: IconDef = &[
    b(Loop(&[(2.0, 8.0), (22.0, 8.0), (22.0, 16.0), (2.0, 16.0)])),
    a(Line(&[(6.0, 8.0), (6.0, 16.0)])),
    a(Line(&[(12.0, 8.0), (12.0, 16.0)])),
    a(Line(&[(18.0, 8.0), (18.0, 16.0)])),
];
const STUD_GRID: IconDef = &[
    b(Line(&[(5.0, 3.0), (5.0, 21.0)])),
    b(Line(&[(12.0, 3.0), (12.0, 21.0)])),
    b(Line(&[(19.0, 3.0), (19.0, 21.0)])),
    a(Line(&[(5.0, 12.0), (12.0, 12.0)])),
    a(Dot(5.0, 12.0, 1.3)),
];
const WALL_FIT: IconDef = &[
    b(Line(&[(2.0, 9.0), (22.0, 9.0)])),
    b(Line(&[(2.0, 15.0), (22.0, 15.0)])),
    a(Line(&[(12.0, 2.0), (12.0, 7.5)])),
    a(Line(&[(10.0, 5.5), (12.0, 7.5), (14.0, 5.5)])),
    a(Line(&[(12.0, 22.0), (12.0, 16.5)])),
    a(Line(&[(10.0, 18.5), (12.0, 16.5), (14.0, 18.5)])),
];
const WALL_RUN: IconDef = &[
    b(Loop(&[(10.0, 2.0), (16.0, 2.0), (16.0, 22.0), (10.0, 22.0)])),
    a(Line(&[(4.0, 20.0), (4.0, 4.0)])),
    a(Line(&[(2.0, 7.0), (4.0, 4.0), (6.0, 7.0)])),
];
const WALL_CONFLICTS: IconDef =
    &[b(Loop(&[(2.0, 9.0), (22.0, 9.0), (22.0, 15.0), (2.0, 15.0)])), a(Line(&[(8.0, 4.0), (16.0, 20.0)])), a(Line(&[(16.0, 4.0), (8.0, 20.0)]))];
const STUD_MOVE: IconDef = &[
    b(Line(&[(5.0, 4.0), (5.0, 20.0)])),
    b(Line(&[(19.0, 4.0), (19.0, 20.0)])),
    a(Line(&[(12.0, 4.0), (12.0, 20.0)])),
    a(Line(&[(8.0, 12.0), (16.0, 12.0)])),
    a(Line(&[(14.0, 10.0), (16.0, 12.0), (14.0, 14.0)])),
];

// Project.
const FOLDER: &[(f32, f32)] = &[(2.0, 5.0), (9.0, 5.0), (11.0, 7.0), (22.0, 7.0), (22.0, 20.0), (2.0, 20.0)];
const START_PROJECT: IconDef = &[b(Loop(FOLDER)), a(Line(&[(12.0, 10.0), (12.0, 17.0)])), a(Line(&[(8.5, 13.5), (15.5, 13.5)]))];
const DXF_IMPORT: IconDef = &[
    b(Loop(&[(6.0, 2.0), (16.0, 2.0), (20.0, 6.0), (20.0, 22.0), (6.0, 22.0)])),
    a(Line(&[(1.0, 14.0), (12.0, 14.0)])),
    a(Line(&[(9.0, 11.0), (12.0, 14.0), (9.0, 17.0)])),
];
const NEW_SHEET: IconDef = &[
    b(Loop(&[(2.0, 5.0), (22.0, 5.0), (22.0, 19.0), (2.0, 19.0)])),
    b(Line(&[(17.0, 5.0), (17.0, 19.0)])),
    a(Loop(&[(4.5, 7.5), (14.5, 7.5), (14.5, 16.5), (4.5, 16.5)])),
];

/// The LAMB tab: everything a user would start from View ▸ LAMB, grouped by what it works on.
pub const LAMB_TAB: ExtTab = ExtTab {
    name: "LAMB",
    groups: &[
        ExtGroup {
            name: "Views",
            large: &[
                tool(NEW_PLAN, "newplan", "New Plan"),
                tool(NEW_SECTION_LINE, "newsectionline", "New Section Line"),
                tool(PLAN_HERE, "newplanline", "Plan Here"),
                tool(EDIT_VIEW, "editview", "Edit View"),
            ],
            small: &[
                tool(NEW_OFFSET_PLAN, "newoffsetplan", "New Offset Plan"),
                tool(NEW_ELEVATION_LINE, "newelevationline", "New Elevation Line"),
                tool(NEW_DETAIL, "newdetail", "New Detail"),
                tool(DUPLICATE_VIEW, "duplicateview", "Duplicate View"),
                tool(UNPLACE_VIEW, "unplaceview", "Unplace View"),
                tool(VIEW_PROPS, "viewprops", "View Properties"),
                tool(LEVEL, "level", "Level"),
                tool(CUT_LINE, "cutline", "Cut Line"),
            ],
        },
        ExtGroup {
            name: "Links",
            large: &[tool(LINK, "link", "Link"), tool(UNLINK, "unlink", "Unlink"), tool(RESOLVE, "resolve", "Review Issues")],
            small: &[
                tool(ACCEPT, "accept", "Accept AI Line"),
                tool(SET_ROLE, "setrole", "Set Role"),
                tool(LAYER_ROLE, "layerrole", "Layer Default Role"),
            ],
        },
        ExtGroup {
            name: "Panes",
            large: &[tool(REF_PANE, "refpane", "Reference Pane"), tool(UNDERLAY, "underlay", "Underlay"), tool(PANE_LOCK, "panelock", "Pane Lock")],
            small: &[tool(UCS_ALIGN, "ucsalign", "UCS Align")],
        },
        ExtGroup {
            name: "Details",
            large: &[
                tool(DETAIL_TAG, "detailtag", "Detail Tag"),
                tool(DETAIL_REF, "detailref", "Detail Reference"),
                tool(REPEAT, "repeat", "Repeat"),
                tool(OUTSIDE_POINT, "outsidepoint", "Outside Point"),
            ],
            small: &[tool(DETAIL_TAGS, "detailtags", "Detail Tags Report")],
        },
        ExtGroup {
            name: "Assembly",
            large: &[
                tool(ASSEMBLY_PLAN, "assemblyplan", "Assembly Plan"),
                tool(STUD_GRID, "studgrid", "Stud Grid"),
                tool(WALL_FIT, "wallfit", "Wall Fit"),
            ],
            small: &[
                tool(WALL_RUN, "wallrun", "Wall Run"),
                tool(WALL_CONFLICTS, "wallconflicts", "Wall Conflicts"),
                tool(STUD_MOVE, "studmove", "Move Stud"),
            ],
        },
        ExtGroup {
            name: "Project",
            large: &[tool(START_PROJECT, "lambproject", "Start Project")],
            small: &[tool(DXF_IMPORT, "dxfimport", "Import DXF as View"), tool(NEW_SHEET, "newsheet", "New Sheet")],
        },
    ],
};

#[cfg(test)]
mod tests {
    use super::*;
    use cadcraft_ui_egui::palettes::{DRAFTING, MODELING};

    fn tools() -> impl Iterator<Item = &'static ExtTool> {
        LAMB_TAB.groups.iter().flat_map(|g| g.large.iter().chain(g.small.iter()))
    }

    #[test]
    fn every_lamb_tool_names_a_command_and_a_legible_distinct_icon() {
        assert!(lamb_cadcraft::commands::install_app().is_ok());
        for t in tools() {
            // A tip is the command's own title; the palette greys a command that is not there.
            let spec = cadcraft_engine::command_specs().iter().find(|s| s.id == t.command);
            assert!(cadcraft_engine::find_command(t.command).is_some(), "{} has no command", t.command);
            if let Some(spec) = spec {
                assert_eq!(spec.label, t.tip, "{}", t.command);
            }
            assert!(!t.icon.is_empty(), "{} has no icon", t.command);
            let inside = |x: f32, y: f32| (0.0..=24.0).contains(&x) && (0.0..=24.0).contains(&y);
            for s in t.icon {
                let ok = match s.path {
                    IconPath::Line(p) | IconPath::Loop(p) => p.len() >= 2 && p.iter().all(|&(x, y)| inside(x, y)),
                    IconPath::Circle(x, y, r) | IconPath::Dot(x, y, r) | IconPath::Arc(x, y, r, _, _) => inside(x - r, y - r) && inside(x + r, y + r),
                };
                assert!(ok, "{}: {:?} leaves the 24 grid", t.command, s.path);
            }
        }
        let all: Vec<&ExtTool> = tools().collect();
        for (i, t) in all.iter().enumerate() {
            assert!(all[i + 1..].iter().all(|u| u.icon != t.icon && u.command != t.command), "{} repeats", t.command);
        }
        for g in LAMB_TAB.groups {
            assert!(g.large.len() <= 4, "{}", g.name);
        }
    }

    #[test]
    fn typed_only_commands_stay_off_the_lamb_tab() {
        for t in tools() {
            assert!(!["showlinks", "lambopen", "lambsave"].contains(&t.command), "{} is typed-only", t.command);
        }
    }

    #[test]
    fn lamb_group_names_do_not_collide_with_the_built_in_tabs() {
        for g in LAMB_TAB.groups {
            assert!(DRAFTING.iter().chain(MODELING).all(|(n, _, _)| *n != g.name), "{}", g.name);
        }
    }
}
