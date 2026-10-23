# Guidance for coding agents (and contributors) working on win-test-lab

## What this is

A tool to test Windows software on QEMU/KVM: on one or more standalone Windows machines
(workgroup mode) or inside a small Active Directory domain (AD mode: one machine has
`role: "dc"`, any Server version; the others join, any OS). A lab is `labs/<name>.json` or a
`lab.json` in another project, built once into a frozen "0 state", and reset to it before
every run. A run config (`examples/configs/*.json`, or a project's `wintest/*.json`) names
its lab by name or path, mounts host folders and runs programs on chosen machines as chosen
users. The user wants it simple: "0 state, run my program, see if it works, restore, deploy
the new version". No snapshot juggling.

The host side is the Rust program in `wtl-rs/` (binary `wtl-rs`, which acts as `lab` when
called by that name). The guest side is PowerShell in `guest/`. They talk over a serial log
(SPEC.md). `build-image.sh` (bash + `lib/*.py`) still installs the Windows base images; `lab`
calls it. `launch-test.sh` is a separate, older single-VM tester that shares those helpers.

Read [SPEC.md](../SPEC.md) for the design and [docs/CONFIG.md](CONFIG.md) for the formats.

## Hard constraints

- **Guest PowerShell must be 4.0 / .NET 4.5 compatible** (Server 2012 R2 is the floor): no
  `Expand-Archive`, `::new()`, classes or `-AsHashtable`. Run `python3 tests/ps_lint.py
  guest/*.ps1` after editing any guest script.
- **Only the agent writes COM1.** The SYSTEM worker (`labsvc.ps1`) answers through files under
  `C:\WTL\svc`; output reaches the host as `OUTPUT` events sent by the agent's session.
- **No root and no Samba on the host.** Host folders reach the guests as read-only ISOs
  written by `wtl-rs/src/mkiso.rs`; the lab LAN is QEMU `-netdev dgram` multicast on loopback.
- **Keep the formats strict.** `labdef.rs` uses `deny_unknown_fields` and validates before
  anything boots. A new field needs: the struct field with a doc comment (it becomes the
  schema description), validation in `resolve()`, `wtl-rs schema` to regenerate
  `wtl-rs/schema/`, and a line in docs/CONFIG.md.
- **Use crates for generic work** (JSON, CLI parsing, errors, time, Ctrl-C, base64). The
  win-test-lab logic itself (ISO writer, QMP client, serial protocol, runner) is in-tree.
- At most 3 VMs at a time on the development machine.

## Where things are

    wtl-rs/src/lab.rs        `lab` CLI (run/up/restore/stop/status/prepare/rebuild); find_root;
                             build_base_images (drives build-image.sh)
    wtl-rs/src/labdef.rs     LabDef/RunDef (raw serde) -> Lab/Run (resolved, validated)
    wtl-rs/src/labrun.rs     MultiLab: 0 state, boot, control + mount discs, steps, result.json
    wtl-rs/src/qemu.rs       Env (WTL_* settings) and the QEMU argv
    wtl-rs/src/{qmp,serialwait,outputs,mkiso,oslist,sys,util,logx,pyjson,schema}.rs
    wtl-rs/tests/fake_lab.rs end-to-end suite; one binary that is the test driver, a fake
                             qemu-system-x86_64 (simulates guest/lab.ps1 from the control disc)
                             and a fake qemu-img
    guest/lab.ps1            rename, DC promotion, users/groups, join, autologon, run steps
    guest/labsvc.ps1         SYSTEM worker: mounts, steps as system / as another user
    guest/agent.ps1          logon agent (COM1); also the launch-test.sh agent
    guest/setup.ps1          base-image first logon
    labs/*.json, examples/configs/*.json, wtl-rs/schema/*.json

State on disk (git-ignored): `images/` (base images), `labs/state/<lab>/` (0 state, overlays,
serial logs, `runs/<id>/`), `iso-files/`, `runs/` (build-image.sh scratch).

## When you change things

- `guest/lab.ps1`: shipped on the control disc at every boot, so the next `lab run` uses it,
  except what the 0-state build bakes in (rename, promotion, users, join, autologon, the
  `WTL-Svc` task): for those, `lab rebuild <lab>` and run again.
- `guest/labsvc.ps1`: copied into the 0 state, so `lab rebuild <lab>`.
- `guest/setup.ps1`, `guest/autounattend*.xml.tmpl`, `build-image.sh`: these make the base
  images; delete `images/<os>-*.qcow2` (or `build-image.sh --force`) and the lab's 0 state.
- `wtl-rs/src/*`: `cargo build --release` in `wtl-rs/` (`~/.local/bin/lab` points at
  `target/release/wtl-rs`).
- The serial protocol, the control disc's `lab.json` and the `WTL_*` variables are the
  contract between host and guest: change both sides together, and SPEC.md.

## Before saying it works

    tests/all.sh                         # everything that needs no VM (cargo test, ps_lint,
                                         # lab/config checks, the fake-qemu suite)
    cd wtl-rs && cargo clippy --all-targets && cargo fmt --check
    cargo check --target x86_64-pc-windows-gnu && cargo check --target x86_64-apple-darwin
    RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
    wtl-rs/tests/parity.sh               # mkiso / outputs agree with lib/*.py (needs python3)

Anything touching the guest or the boot sequence also needs a real run, in each mode it
affects: `lab rebuild single-2019 && lab run examples/configs/single-2019-smoke.json`
(workgroup, ~10 min) and `lab rebuild mixed && lab run examples/configs/mixed-smoke.json`
(AD, ~30 min).

## Gotchas that cost time before

- Passwords containing the account name are rejected by Windows.
- Domain autologon needs the LSA secret `DefaultPassword`; it sometimes stops at the sign-in
  screen, so the host resets a member that does not log on within 5 min.
- AD Web Services is not up right after the DC boots (retry); 2016/2019 DCs need ~10 min after
  promotion before joins work (the join retries for 20 min).
- A plain domain user cannot call `Stop-Computer`: the host powers machines off over ACPI.
- `Add-ADGroupMember` needs the group to exist: `New-LabUser` creates missing groups.
- The 0-state build must boot the `-0.qcow2` disks themselves, not an overlay on them, or the
  domain ends up in a throwaway disk.
- `setup.ps1` must install the `WTL-Agent` autostart after `BUILD_DONE`: an agent started
  during the same logon holds COM1 and the host never sees `BUILD_DONE`.
- Steps "as" another user: `Process.Start` with credentials (CreateProcessWithLogonW) does not
  work from LocalSystem; labsvc.ps1 uses `LogonUser` + `CreateProcessAsUser`, with the output
  files opened by SYSTEM and inherited (the user has no rights on `C:\WTL`).
- A lab file outside `labs/` gets a state name with a hash of its path: moving a project's
  `lab.json` means a new 0 state (or set `"name"`).
