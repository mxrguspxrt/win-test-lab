//! win-test-lab host side: the Active Directory lab runner and the pieces it is built from.
//!
//! * [`lab`]: the `lab` command line, and shared helpers (finding the checkout, installing
//!   Windows base images with `build-image.sh`).
//! * [`labdef`]: the typed JSON lab and run formats, their validation and JSON Schema.
//! * [`labrun`]: the runner: builds a lab's 0 state, boots its machines on a private
//!   multicast LAN, runs a config's steps on each machine and collects the results.
//! * The building blocks: [`qemu`] (settings + command line), [`qmp`] (the QEMU monitor),
//!   [`serialwait`] / [`outputs`] (the guest's serial event protocol), [`mkiso`] (the
//!   control and mount discs), [`oslist`] (the OS table), [`sys`] (processes, Ctrl-C),
//!   [`schema`], [`logx`], [`pyjson`], [`util`].

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
pub mod build;
pub mod util;
pub mod winiso;
