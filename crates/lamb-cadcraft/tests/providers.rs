//! Command providers (FORK.md patch 4): a crate outside `cadcraft-engine` adds commands and
//! overrides a built-in by id. The registry is process-global, so this lives in its own test binary
//! and uses a single test fn (tests in one binary share the global and run in parallel).

use cadcraft_engine::cmd::always;
use cadcraft_engine::{CommandSpec, Result, Session, command_specs, register_command_provider};
use serde_json::{Value, json};

fn lamb_fillet(_: &mut Session, _: &Value) -> Result<Value> {
    Ok(json!("lamb-fillet"))
}

fn lamb_ping(_: &mut Session, _: &Value) -> Result<Value> {
    Ok(json!("pong"))
}

fn provider() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("fillet", "Fillet", lamb_fillet).menu(&["Modify", "Fillet"]).alias(&["f"]).enabled(always).noundo(),
        CommandSpec::new("lamb.ping", "Ping", lamb_ping).enabled(always).noundo(),
    ]
}

#[test]
fn provider_overrides_in_place_and_appends() -> std::result::Result<(), Box<dyn std::error::Error>> {
    register_command_provider(provider)?;

    let mut s = Session::new();
    assert_eq!(s.execute("fillet", &json!({}))?, json!("lamb-fillet"));
    assert_eq!(s.execute("lamb.ping", &json!({}))?, json!("pong"));

    let specs = command_specs();
    let fillets: Vec<usize> = specs.iter().enumerate().filter(|(_, c)| c.id == "fillet").map(|(i, _)| i).collect();
    assert_eq!(fillets.len(), 1, "exactly one fillet");
    assert_eq!(specs.get(fillets[0] + 1).map(|c| c.id), Some("chamfer"), "override keeps the built-in's slot");
    assert_eq!(specs.last().map(|c| c.id), Some("lamb.ping"), "new ids are appended");

    assert_eq!(register_command_provider(provider), Err("command registry already built"));
    Ok(())
}
