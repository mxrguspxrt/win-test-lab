//! wtl-rs - the win-test-lab host side as one program.
//!
//!     wtl-rs lab CMD ...          the AD lab runner (same CLI as lab.sh); also what runs
//!                                 when the binary is invoked as `lab` (e.g. via a symlink)
//!     wtl-rs labconf json|check CONFIG...
//!     wtl-rs mkiso -o OUT.iso -V VOLID SRC_DIR
//!     wtl-rs qmp SOCK CMD ...
//!     wtl-rs serialwait LOG --until EV ...
//!     wtl-rs outputs SERIAL_LOG RESULTS_DIR

use clap::{Parser, Subcommand};
use wtl_rs::{lab, labconf, mkiso, outputs, qmp, schema, serialwait, util};

#[derive(Parser, Debug)]
#[command(name = "wtl-rs", version, about = "win-test-lab host tools (Rust port of lab.sh + lib/*.py)")]
struct Cli {
    #[command(subcommand)]
    tool: Tool,
}

#[derive(Subcommand, Debug)]
enum Tool {
    /// the AD lab runner: run / up / restore / stop / status / prepare / rebuild (lab.sh). A
    /// copy or symlink of this binary named `lab` behaves as `wtl-rs lab`.
    Lab(lab::Cli),
    Labconf(labconf::Cli),
    Mkiso(mkiso::Cli),
    Qmp(qmp::Cli),
    Serialwait(serialwait::Cli),
    Outputs(outputs::Cli),
    Schema(schema::Cli),
}

/// What runs when the binary is called `lab`.
#[derive(Parser, Debug)]
#[command(name = "lab", version)]
struct LabOnly {
    #[command(flatten)]
    lab: lab::Cli,
}

/// Print a clap error / help and exit: help and --version exit 0; a usage error exits with
/// CODE (2 for lab, like lab.sh; 64 for the tools, like the Python scripts).
fn usage_exit(e: clap::Error, code: i32) -> ! {
    let _ = e.print();
    std::process::exit(if e.use_stderr() { code } else { 0 })
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

fn main() {
    let argv: Vec<String> = std::env::args().collect();
    let prog = argv.first().map(|a| util::stem(util::basename(a)).to_ascii_lowercase()).unwrap_or_default();
    let code = if prog == "lab" {
        let a = LabOnly::try_parse_from(&argv).unwrap_or_else(|e| usage_exit(e, 2));
        lab::main(&a.lab)
    } else {
        let usage_code = if argv.get(1).map(String::as_str) == Some("lab") { 2 } else { 64 };
        let cli = Cli::try_parse_from(&argv).unwrap_or_else(|e| usage_exit(e, usage_code));
        match &cli.tool {
            Tool::Lab(a) => lab::main(a),
            Tool::Labconf(a) => labconf::cli(a),
            Tool::Mkiso(a) => report("mkiso", mkiso::cli(a)),
            Tool::Qmp(a) => qmp::cli(a),
            Tool::Serialwait(a) => serialwait::cli(a),
            Tool::Outputs(a) => report("outputs", outputs::cli(a)),
            Tool::Schema(a) => report("schema", schema::cli(a)),
        }
    };
    std::process::exit(code);
}
