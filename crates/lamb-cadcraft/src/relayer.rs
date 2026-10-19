//! Link maintenance step 4: a layer change carries the entity's automatic-link component with it
//! (TS `entityProps.ts` SETLAYER, propagate on). Whatever command changed the layer —
//! `properties.set`, MATCHPROP, LAYCUR, the Properties palette — every member of the changed
//! entity's [`linked_entity_component`] that is not itself changed moves to the same layer. An
//! explicit link stops the walk; a member reached from changed entities with different new layers
//! is left alone. Layer lock is not checked (TS parity).

use std::collections::BTreeMap;

use cadcraft_doc::{Drawing, Handle};
use lamb_core::derivation::linking::linked_entity_component;
use lamb_core::derivation::read::ProjectRead;
use lamb_core::model::Id;

use crate::project::DrawingProject;
use crate::schema::SchemaError;

/// Entity id → CADCraft layer name, over every view block.
fn layer_map(p: &DrawingProject<'_>) -> BTreeMap<Id, String> {
    let mut out = BTreeMap::new();
    for v in p.views() {
        for e in p.entities(v.id) {
            out.insert(e.id, e.layer_id.to_owned());
        }
    }
    out
}

/// Moves the automatic-link component of every view-block entity whose layer differs between
/// `before` and `after` onto its new layer. Returns the entities relayered, in id order.
pub(crate) fn cascade_layers(before: &Drawing, after: &mut Drawing) -> Result<Vec<Id>, SchemaError> {
    let writes: BTreeMap<Id, String> = {
        let old = layer_map(&DrawingProject::new(before)?);
        let p = DrawingProject::new(after)?;
        let new = layer_map(&p);
        let changed: BTreeMap<Id, &String> =
            new.iter().filter(|(id, layer)| old.get(id).is_some_and(|o| o != *layer)).map(|(id, l)| (*id, l)).collect();
        if changed.is_empty() {
            return Ok(Vec::new());
        }
        // Member → its target layer; `None` once two changed entities disagree.
        let mut targets: BTreeMap<Id, Option<&String>> = BTreeMap::new();
        for (id, layer) in &changed {
            for m in linked_entity_component(&p, *id) {
                if changed.contains_key(&m) {
                    continue;
                }
                targets
                    .entry(m)
                    .and_modify(|t| {
                        if t.is_some_and(|t| t != *layer) {
                            *t = None;
                        }
                    })
                    .or_insert(Some(*layer));
            }
        }
        targets
            .into_iter()
            .filter_map(|(m, t)| {
                let t = t?;
                (new.get(&m).is_some_and(|cur| cur != t)).then(|| (m, t.clone()))
            })
            .collect()
    };
    for (id, layer) in &writes {
        after
            .modify_entity(Handle(id.0), |e| e.common.layer = layer.clone())
            .map_err(|e| SchemaError::Malformed { what: "link maintenance".into(), message: e.to_string() })?;
    }
    Ok(writes.into_keys().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap as Map;

    use lamb_core::model::{LinkOrigin, Project};
    use lamb_core::persistence::files_to_project;

    use crate::import::project_to_drawing;
    use crate::schema::read_document;

    fn fixture() -> Drawing {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../lamb-core/tests/lamb/u-wall-two-sections.lamb.json");
        let files: Map<String, String> = serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("json");
        let project: Project = files_to_project(&files).expect("project");
        project_to_drawing(&project).expect("import").0
    }

    fn layer_of(d: &Drawing, id: Id) -> String {
        let p = DrawingProject::new(d).expect("project");
        p.entity(id).expect("entity").layer_id.to_owned()
    }

    fn set_layer(d: &mut Drawing, id: Id, layer: &str) {
        d.modify_entity(Handle(id.0), |e| e.common.layer = layer.to_owned()).expect("modify");
    }

    #[test]
    fn no_layer_change_relayers_nothing() {
        let d = fixture();
        let mut after = d.clone();
        assert!(cascade_layers(&d, &mut after).expect("cascade").is_empty());
        assert!(after == d);
    }

    #[test]
    fn a_changed_entity_carries_its_component() {
        let d = fixture();
        let links = read_document(&d).expect("doc").links;
        assert!(links.iter().all(|l| l.origin == LinkOrigin::Automatic));
        let e = links[0].a.entity_id;
        let component = linked_entity_component(&DrawingProject::new(&d).expect("p"), e);
        assert!(component.len() > 1);
        let mut after = d.clone();
        set_layer(&mut after, e, "NEW");
        let moved = cascade_layers(&d, &mut after).expect("cascade");
        let expected: Vec<Id> = component.iter().copied().filter(|m| *m != e).collect();
        assert_eq!(moved, expected);
        for m in component {
            assert_eq!(layer_of(&after, m), "NEW");
        }
    }

    #[test]
    fn disagreeing_changes_leave_the_rest_alone() {
        let d = fixture();
        let links = read_document(&d).expect("doc").links;
        let (a, b) = (links[0].a.entity_id, links[0].b.entity_id);
        let mut after = d.clone();
        set_layer(&mut after, a, "ONE");
        set_layer(&mut after, b, "TWO");
        assert!(cascade_layers(&d, &mut after).expect("cascade").is_empty());
    }
}
