# Guidance for coding agents (and contributors) working on win-test-lab

## What this is

A tool to test Windows software on QEMU/KVM: on one or more standalone Windows machines
(workgroup mode) or inside a small Active Directory domain (AD mode: one machine has the
`ad-ds` role, any Server version; the others join, any OS). Machines can carry extra server
roles (`ad-cs`, `dhcp`, `iis`), Windows features and per-machine setup scripts, all baked into
the 0 state. A lab is `labs/<name>.json` or a `lab.json` in another project, built once into a
frozen "0 state", and reset to it before every run. A run config (`examples/configs/*.json`,
or a project's `wintest/*.json`) names its lab by name or path, mounts host folders and runs
programs on chosen machines as chosen users. Keep it simple: 0 state, run the program, see
if it works, restore, deploy the new version. No snapshot juggling.

The host side is the Rust crate at the repository root (crate `win-test-lab` on crates.io,
binary `wtl`). The guest side is PowerShell in `guest/`, compiled into `wtl` (with
`lib/oslist.conf` and `labs/*.json`) so an installed `wtl` needs no checkout; run from a
checkout, `wtl` reads them from disk instead. They talk over a serial log (SPEC.md).

Read [SPEC.md](../SPEC.md) for the design and [docs/CONFIG.md](CONFIG.md) for the formats.

## Hard constraints

- **Guest PowerShell must be 4.0 / .NET 4.5 compatible** (Server 2012 R2 is the floor): no
  `Expand-Archive`, `::new()`, classes or `-AsHashtable`. Run `python3 tests/ps_lint.py
  guest/*.ps1` after editing any guest script.
- **Only the agent writes COM1.** The SYSTEM worker (`labsvc.ps1`) answers through files under
  `C:\WTL\svc`; output reaches the host as `OUTPUT` events sent by the agent's session.
- **No root and no Samba on the host.** Host folders reach the guests as read-only ISOs
  written by `src/mkiso.rs`; the lab LAN is QEMU `-netdev dgram` multicast on loopback.
- **Keep the formats strict.** `labdef.rs` uses `deny_unknown_fields` and validates before
  anything boots. A new field needs: the struct field with a doc comment (it becomes the
  schema description), validation in `resolve()`, `cargo run -- schema` to regenerate
  `schema/`, and a line in docs/CONFIG.md.
- **Use crates for generic work** (JSON, CLI parsing, errors, time, Ctrl-C, base64, per-user
  folders). The win-test-lab logic itself (ISO writer, QMP client, serial protocol, runner)
  is in-tree.
- **Nothing outside the crate at runtime.** A file `wtl` needs goes through `src/assets.rs`
  (compiled in by `embed.rs`) and the package's `include` list in Cargo.toml; data paths go
  through `src/home.rs`. Check with `cargo package --list` and a `cargo install --path .`
  run from an empty folder.
- Mind the host: each VM takes 3-4 GB of RAM and 2 vCPUs; a 3-machine lab wants ~12 GB free.

## Where things are

    src/main.rs              the `wtl` command line
    src/lab.rs               the lab commands (run/up/restore/stop/status/prepare/rebuild,
                             doctor, check-iso, home, init); build_base_images
    src/home.rs              the home: --home / WTL_HOME / wtl.json / user config / data folder
    src/assets.rs, embed.rs  guest/, lib/oslist.conf, labs/*.json built into the program
    src/build.rs             `wtl build-image`: an unattended Windows install
    src/labdef.rs            LabDef/RunDef (raw serde) -> Lab/Run (resolved, validated)
    src/labrun.rs            MultiLab: 0 state, boot, control + mount discs, steps, result.json
    src/qemu.rs              Env (WTL_* settings) and the QEMU argv
    src/{qmp,serialwait,outputs,mkiso,winiso,oslist,sys,util,logx,pyjson,schema,sha256}.rs
    tests/fake_lab.rs        end-to-end suite; one binary that is the test driver, a fake
                             qemu-system-x86_64 (simulates guest/lab.ps1 from the control disc)
                             and a fake qemu-img
    guest/lab.ps1            rename, DC promotion, users/groups, join, autologon, the build-time
                             "configure" phase (roles ad-cs/dhcp/iis, features, setup), run steps
    guest/labsvc.ps1         SYSTEM worker: mounts, steps as system / as another user
    guest/agent.ps1          logon agent (COM1)
    guest/setup.ps1          base-image first logon
    labs/*.json, examples/configs/*.json, schema/*.json

State on disk (git-ignored, in the home; a checkout is its own home): `images/` (base
images), `labs/state/<lab>/` (0 state, overlays, serial logs, `runs/<id>/`), `iso-files/`,
`runs/` (base-image build scratch). `wtl home` shows where each one is.

## When you change things

- `guest/lab.ps1`: shipped on the control disc at every boot, so the next `wtl run` uses it,
  except what the 0-state build bakes in (rename, promotion, users, join, autologon, the
  `WTL-Svc` task): for those, `wtl rebuild <lab>` and run again.
- `guest/labsvc.ps1`: copied into the 0 state, so `wtl rebuild <lab>`.
- `guest/setup.ps1`, `guest/autounattend*.xml.tmpl`: these make the base images; delete
  `images/<os>-*.qcow2` and the lab's 0 state.
- `src/*`: `cargo build --release` (then `target/release/wtl`). A `wtl` built in a checkout
  uses that checkout's guest scripts; an installed one uses the copies built into it.
- The serial protocol, the control disc's `lab.json` and the `WTL_*` variables are the
  contract between host and guest: change both sides together, and SPEC.md.

## Before saying it works

    tests/all.sh                         # everything that needs no VM (cargo test, ps_lint,
                                         # clippy, fmt, lab/config checks)
    cargo clippy --all-targets && cargo fmt --check
    cargo check --target x86_64-pc-windows-gnu && cargo check --target x86_64-apple-darwin
    RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
    cargo package --list                 # what would be published

Anything touching the guest or the boot sequence also needs a real run, in each mode it
affects: `wtl rebuild single-2019 && wtl run examples/configs/single-2019-smoke.json`
(workgroup, ~10 min) and `wtl rebuild mixed && wtl run examples/configs/mixed-smoke.json`
(AD, ~30 min).

Publishing to crates.io only after the maintainer has reviewed the package and said yes: a
published version can never be replaced, only yanked.

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
- A lab file outside the home's `labs/` (and not a built-in lab) gets a state name with a hash
  of its path: moving a project's `lab.json` means a new 0 state (or set `"name"`).
- Roles/features/setup are baked into the 0 state in a build-only `configure` phase, so they
  are on the **build** control disc only (empty on a run disc) and changing them needs
  `wtl rebuild`. A member with config stays the (domain) Administrator for that phase, because
  `ad-cs`/`dhcp`-in-AD need that token, then reboots into its `logon` user. `ad-cs` requires an
  AD lab; roles/features need a server OS (checked before boot). A member has no
  ActiveDirectory PowerShell module: `ad-cs` uses ADSI (RootDSE, the template and
  enrollment-service objects) and GroupPolicy, not `Get-ADDomain`. The built-in `User` template
  is v1 and never auto-enrolls: hence the v2 copy `WTLUser`. `Add-CATemplate` right after
  creating a template fails (the CA has not seen it); writing `certificateTemplates` directly
  and restarting certsvc works. `Add-Computer -NewName` can join and still throw ("directory
  service is busy" on the rename): the join loop checks `PartOfDomain`. The pki lab/smoke
  (`labs/pki.json`, `examples/configs/pki-smoke.json`, `examples/pki/`) is the worked example.
