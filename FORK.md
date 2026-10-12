# FORK.md

This is the LAMB fork of [CADCraft](https://github.com/storytold/cadcraft): LAMB (a 2D-first
architectural drafting tool with views, levels, links and derived 3D) rebuilt on CADCraft's
headless crates. LAMB's own code lives in `crates/lamb-core` (no CADCraft dependency) and
`crates/lamb-cadcraft` (the adapter on `cadcraft-engine`). The `lamb` branch tracks upstream `main`
and takes upstream changes by **merge, never rebase**, so every patch to CADCraft's own crates stays
visible and is listed below with its upstream PR.

| Patch | Files | Why | Upstream PR |
|---|---|---|---|
| 2. Extension slots (in-memory) | `crates/doc/src/lib.rs`, `crates/doc/src/entity.rs`, `crates/doc/src/tables.rs`, `crates/engine/src/lib.rs`, `crates/engine/src/ext_tests.rs` | `pub type Extension = BTreeMap<String, serde_json::Value>` keyed by application name (the DXF APPID idea); `pub ext: Extension` on `Common`, `Layer` and `Drawing` (serde `default` + `skip_serializing_if = is_empty`, so existing serialized forms are unchanged while empty). Entity copies (COPY, MIRROR, ARRAYRECT, OFFSET, ROTATE copy, COPYCLIP/PASTECLIP) clone it and undo/redo restore it through the `Arc<Drawing>` snapshots; LAMB keeps its data under `ext["LAMB"]`. DXF I/O ignores it for now (round-trip is a follow-on). | not yet submitted |
| 3. After-command observers | `crates/engine/src/lib.rs`, `crates/engine/src/observer_tests.rs` | `pub observers: Vec<AfterCommand>` on `Session` (`AfterCommand = fn(&mut Session, &Arc<Drawing>)`, the drawing before the command). Called in registration order after CADCraft's own group/constraint/associativity passes in `execute` (undoable, successful commands on the same drawing) and `finish`, only when the drawing changed, so their edits share the command's undo step; undo/redo never trigger them. How LAMB runs link maintenance, solid-cut reconciliation and derived-view regeneration. | not yet submitted |
| 4. Command providers | `crates/engine/src/cmd/mod.rs`, `crates/engine/src/lib.rs` | `register_command_provider(fn() -> Vec<CommandSpec>)` lets a crate outside the engine add commands; a provider spec whose id matches a built-in replaces it in place (same menu slot), a new id is appended, last provider wins. Process-global; registering after the first `command_specs()` call returns `Err`. How LAMB registers `NEWPLAN`, `PLACEVIEW` and its own `fillet`. | not yet submitted |
