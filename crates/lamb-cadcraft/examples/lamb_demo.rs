//! A LAMB demo drawing for looking at fork GUI work in the real app:
//! `cargo run -p lamb-cadcraft --example lamb_demo -- <out.dxf> [fixture]` imports
//! `crates/lamb-core/tests/lamb/<fixture>.lamb.json` (default `u-wall-two-sections`) with
//! `project_to_drawing`, writes it as DXF, reads it back as a self-check and lists the LAMB views
//! (open the file, then `EDITVIEW <name>`).

use std::collections::BTreeMap;
use std::path::Path;

use lamb_cadcraft::{project_to_drawing, view_block_name};
use lamb_core::model::Project;
use lamb_core::persistence::files_to_project;

type R<T> = Result<T, Box<dyn std::error::Error>>;

fn load(name: &str) -> R<Project> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../lamb-core/tests/lamb").join(format!("{name}.lamb.json"));
    let files: BTreeMap<String, String> = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    Ok(files_to_project(&files).map_err(|e| format!("{e:?}"))?)
}

fn main() -> R<()> {
    let mut args = std::env::args().skip(1);
    let out = args.next().ok_or("usage: lamb_demo <out.dxf> [fixture]")?;
    let fixture = args.next().unwrap_or_else(|| "u-wall-two-sections".into());
    let project = load(&fixture)?;
    let (d, map, _) = project_to_drawing(&project).map_err(|e| format!("{e:?}"))?;
    std::fs::write(&out, cadcraft_io::write_dxf(&d))?;
    let back = cadcraft_io::read_dxf(&std::fs::read(&out)?).map_err(|e| format!("{e:?}"))?;
    println!("{out}: {fixture}");
    for v in &project.views {
        let block = view_block_name(map.get(v.id()).ok_or("view not in the id map")?);
        let ok = back.blocks.contains_key(&block);
        println!("  view {:?} -> {block} {}", v.name(), if ok { "read back" } else { "MISSING" });
        if !ok {
            return Err(format!("{block} did not survive the DXF round trip").into());
        }
    }
    Ok(())
}
