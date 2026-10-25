//! `wtl schema [DIR]` - write the JSON Schema of the lab and run files, generated from the
//! typed structs in [`crate::labdef`], to `DIR` (default `schema/` in the home).
//!
//! The schemas let an editor validate and autocomplete `labs/*.json` and run `*.json`.

use crate::labdef;
use anyhow::{Context, Result};
use std::path::PathBuf;

/// `wtl schema [DIR]`
#[derive(clap::Args, Debug)]
#[command(about = "Write the JSON Schema for the lab and run files (schema/lab.schema.json, run.schema.json)")]
pub struct Cli {
    /// where to write the schema files (default: schema/ in the home, see `wtl home`)
    #[arg(value_name = "DIR")]
    pub dir: Option<PathBuf>,
}

pub fn cli(a: &Cli) -> Result<()> {
    let dir = match &a.dir {
        Some(d) => d.clone(),
        None => crate::home::Home::resolve()?.dir.join("schema"),
    };
    std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let (lab, run) = labdef::schemas();
    for (name, body) in [("lab.schema.json", &lab), ("run.schema.json", &run)] {
        let p = dir.join(name);
        std::fs::write(&p, format!("{}\n", body)).with_context(|| format!("cannot write {}", p.display()))?;
        println!("wrote {}", p.display());
    }
    Ok(())
}
