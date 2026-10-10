# FORK.md

This is the LAMB fork of [CADCraft](https://github.com/storytold/cadcraft): LAMB (a 2D-first
architectural drafting tool with views, levels, links and derived 3D) rebuilt on CADCraft's
headless crates. LAMB's own code lives in `crates/lamb-core` (no CADCraft dependency) and
`crates/lamb-cadcraft` (the adapter on `cadcraft-engine`). The `lamb` branch tracks upstream `main`
and takes upstream changes by **merge, never rebase**, so every patch to CADCraft's own crates stays
visible and is listed below with its upstream PR.

| Patch | Files | Why | Upstream PR |
|---|---|---|---|
| 4. Command providers | `crates/engine/src/cmd/mod.rs`, `crates/engine/src/lib.rs` | `register_command_provider(fn() -> Vec<CommandSpec>)` lets a crate outside the engine add commands; a provider spec whose id matches a built-in replaces it in place (same menu slot), a new id is appended, last provider wins. Process-global; registering after the first `command_specs()` call returns `Err`. How LAMB registers `NEWPLAN`, `PLACEVIEW` and its own `fillet`. | not yet submitted |
