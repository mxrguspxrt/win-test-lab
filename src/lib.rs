//! win-test-lab: test Windows software on real Windows machines in QEMU. The `wtl` program
//! and the pieces it is built from.
//!
//! * [`lab`]: the `wtl` command line for labs (run, up, restore, status, ...) and installing
//!   the Windows base images.
//! * [`home`]: where the data lives (ISOs, base images, 0 states) and the config files.
//! * [`assets`]: the guest scripts, the OS table and the example labs built into the program.
//! * [`labdef`]: the typed JSON lab and run formats, their validation and JSON Schema.
//! * [`labrun`]: the runner: builds a lab's 0 state, boots its machines on a private
//!   multicast LAN, runs a config's steps on each machine and collects the results.
//! * [`build`]: an unattended Windows install into a base image.
//! * The building blocks: [`qemu`] (settings + command line), [`qmp`] (the QEMU monitor),
//!   [`serialwait`] / [`outputs`] (the guest's serial event protocol), [`mkiso`] (the
//!   control and mount discs), [`winiso`] (reading Windows ISOs), [`oslist`] (the OS table),
//!   [`sys`] (processes, Ctrl-C), [`schema`], [`sha256`], [`logx`], [`pyjson`], [`util`].

pub mod assets;
pub mod build;
pub mod home;
pub mod lab;
pub mod labdef;
pub mod labrun;
pub mod logx;
pub mod mkiso;
pub mod oslist;
pub mod outputs;
pub mod pyjson;
pub mod qemu;
pub mod qmp;
pub mod schema;
pub mod serialwait;
pub mod sha256;
pub mod sys;
pub mod util;
pub mod winiso;
