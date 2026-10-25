//! The `wtl` command line, shared by this crate's `wtl` binary and the `wtl` crate (which is
//! only this, so `cargo install wtl` and `cargo install win-test-lab` give the same program).
//!
//! ```text
//! wtl run CONFIG              restore the lab to its 0 state, boot it, run the steps
//! wtl up|restore|stop|status|prepare|rebuild LAB|CONFIG
//! wtl doctor | check-iso | home | init
//! wtl build-image OS ...      install Windows into a base image (what `run` does for a
//!                             missing one)
//! wtl schema [DIR]            write the JSON Schema of the lab and run files
//! wtl tool mkiso|qmp|serialwait|outputs ...   the building blocks, for debugging
//! ```

use crate::{lab, mkiso, outputs, qmp, schema, serialwait};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "wtl", version, about = "win-test-lab: test Windows software on real Windows in QEMU", after_help = lab::AFTER_HELP)]
struct Cli {
    /// use DIR as the home (where the ISOs, base images and 0 states are); same as WTL_HOME
    #[arg(long, global = true, value_name = "DIR")]
    home: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Top,
}

#[derive(Subcommand, Debug)]
enum Top {
    #[command(flatten)]
    Lab(lab::Cmd),
    BuildImage(lab::BuildCli),
    Schema(schema::Cli),
    /// the building blocks `wtl` is made of, for debugging
    #[command(subcommand)]
    Tool(Tool),
}

#[derive(Subcommand, Debug)]
enum Tool {
    Mkiso(mkiso::Cli),
    Qmp(qmp::Cli),
    Serialwait(serialwait::Cli),
    Outputs(outputs::Cli),
}

/// Print a clap error / help and exit: help and --version exit 0; a usage error exits 2
/// (64 for `wtl tool ...`, whose exit codes 1-4 mean other things).
fn usage_exit(e: clap::Error) -> ! {
    let _ = e.print();
    let tool = std::env::args().skip(1).find(|a| !a.starts_with('-')).as_deref() == Some("tool");
    std::process::exit(if !e.use_stderr() {
        0
    } else if tool {
        64
    } else {
        2
    })
}

/// Run a tool that reports problems as an error: print it (prefixed with NAME unless the
/// message already starts with it), exit 1.
fn report(name: &str, r: anyhow::Result<()>) -> i32 {
    match r {
        Ok(()) => 0,
        Err(e) => {
            let msg = format!("{:#}", e);
            if msg.starts_with(&format!("{}: ", name)) {
                eprintln!("{}", msg);
            } else {
                eprintln!("{}: {}", name, msg);
            }
            1
        }
    }
}

/// Parse the process's arguments, run the command, exit with its code.
pub fn main() -> ! {
    let cli = Cli::try_parse().unwrap_or_else(|e| usage_exit(e));
    if let Some(h) = &cli.home {
        // set before anything resolves the home; child `wtl build-image` runs inherit it
        std::env::set_var("WTL_HOME", crate::util::abspath(h));
    }
    let code = match &cli.cmd {
        Top::Lab(c) => lab::main(c),
        Top::BuildImage(a) => lab::build_main(a),
        Top::Schema(a) => report("schema", schema::cli(a)),
        Top::Tool(Tool::Mkiso(a)) => report("mkiso", mkiso::cli(a)),
        Top::Tool(Tool::Qmp(a)) => qmp::cli(a),
        Top::Tool(Tool::Serialwait(a)) => serialwait::cli(a),
        Top::Tool(Tool::Outputs(a)) => report("outputs", outputs::cli(a)),
    };
    std::process::exit(code);
}
