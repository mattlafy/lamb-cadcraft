//! After-command observers (`Session::observers`, LAMB fork patch 3).

use std::cell::Cell;
use std::sync::Arc;

use cadcraft_doc::{Circle, Drawing, EntityKind};
use cadcraft_geom::Vec3;
use serde_json::json;

use crate::*;

thread_local! {
    static CALLS: Cell<usize> = const { Cell::new(0) };
}

fn calls() -> usize {
    CALLS.with(|c| c.get())
}

fn lines(d: &Drawing) -> usize {
    d.model.iter().filter(|e| matches!(e.kind, EntityKind::Line(_))).count()
}

fn kinds(s: &Session) -> Vec<&'static str> {
    s.doc().unwrap().model.iter().map(|e| e.kind.type_name()).collect()
}

fn radii(s: &Session) -> Vec<f64> {
    s.doc().unwrap().model.iter().filter_map(|e| if let EntityKind::Circle(c) = &e.kind { Some(c.radius) } else { None }).collect()
}

/// Adds a circle of radius `r` when the command added a line.
fn circle_if_line_added(s: &mut Session, before: &Arc<Drawing>, r: f64) {
    CALLS.with(|c| c.set(c.get() + 1));
    let gained = s.doc().is_ok_and(|d| lines(d) > lines(before));
    if gained && let Err(e) = s.add_entity(EntityKind::Circle(Circle { center: Vec3::new(0.0, 0.0, 0.0), radius: r })) {
        s.echo(e.to_string());
    }
}

fn obs_one(s: &mut Session, before: &Arc<Drawing>) {
    circle_if_line_added(s, before, 1.0);
}

fn obs_two(s: &mut Session, before: &Arc<Drawing>) {
    circle_if_line_added(s, before, 2.0);
}

fn session() -> Session {
    let mut s = Session::new();
    s.observers.push(obs_one);
    s
}

#[test]
fn observer_edit_shares_the_commands_undo_step() {
    let mut s = session();
    s.execute("line", &json!({"points": [[0, 0], [10, 0]]})).unwrap();
    assert_eq!(kinds(&s), vec!["Line", "Circle"]);
    assert_eq!(calls(), 1);
    s.undo().unwrap();
    assert_eq!(s.doc().unwrap().model.len(), 0);
}

#[test]
fn observer_runs_after_an_interactive_command() {
    let mut s = session();
    s.cmdline("LINE").unwrap();
    s.cmdline("0,0").unwrap();
    s.cmdline("10,0").unwrap();
    s.cmdline("").unwrap();
    assert!(s.running.is_none());
    assert_eq!(kinds(&s), vec!["Line", "Circle"]);
    assert_eq!(calls(), 1);
    s.undo().unwrap();
    assert_eq!(s.doc().unwrap().model.len(), 0);
}

#[test]
fn undo_and_redo_do_not_run_observers() {
    let mut s = session();
    s.execute("line", &json!({"points": [[0, 0], [10, 0]]})).unwrap();
    assert_eq!(calls(), 1);
    s.undo().unwrap();
    s.redo().unwrap();
    assert_eq!(calls(), 1);
    assert_eq!(kinds(&s), vec!["Line", "Circle"]);
    // Through the command line too.
    s.cmdline("U").unwrap();
    s.cmdline("REDO").unwrap();
    assert_eq!(calls(), 1);
    assert_eq!(kinds(&s), vec!["Line", "Circle"]);
}

#[test]
fn a_command_that_changes_nothing_runs_no_observer() {
    let mut s = session();
    let undo_before = s.state().unwrap().undo.len();
    s.execute("dist", &json!({"p1": [0, 0], "p2": [3, 4]})).unwrap();
    assert_eq!(calls(), 0);
    assert_eq!(s.state().unwrap().undo.len(), undo_before);
}

#[test]
fn observers_run_in_registration_order() {
    let mut s = session();
    s.observers.push(obs_two);
    s.execute("line", &json!({"points": [[0, 0], [10, 0]]})).unwrap();
    assert_eq!(calls(), 2);
    assert_eq!(radii(&s), vec![1.0, 2.0]);
    s.undo().unwrap();
    assert_eq!(s.doc().unwrap().model.len(), 0);
}
