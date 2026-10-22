# wtl-rs progress

Hand-over file: what is done, what is next, why things are the way they are. Update it
after each finished piece. User-facing docs are in README.md.

## Done (2026-10-22)

1. **labconf** (`src/labconf.rs`): same keys, rules, error texts and step order as
   lib/labconf.py. `tests/labconf.rs` = tests/test_labconf.py ported case by case, plus
   exact error messages, edge cases, the JSON form and all examples/configs/*.conf.
2. **mkiso, qmp, serialwait, outputs** (`src/{mkiso,qmp,serialwait,outputs}.rs`). They are
   used in-process by `lab` and also available as `wtl-rs mkiso|qmp|serialwait|outputs` with
   the Python CLIs. mkiso output is byte-identical to mkiso.py apart from timestamps.
3. **lab** (`src/lab.rs` + `src/qemu.rs`, `src/shconf.rs`, `src/oslist.rs`, `src/sys.rs`):
   run / up / restore / stop / status / prepare / rebuild / help-config, the same options,
   messages and exit codes as lab.sh, and the same labs/state/<lab>/ layout. A binary named
   `lab` acts as `wtl-rs lab`.
4. **README.md**: build/use, settings, the platform section (KVM / WHPX / Intel-Mac HVF /
   no Apple Silicon), tests, differences from lab.sh.
5. **Switched to crates** (user's request, relayed by the parent session). The hand-written
   JSON, argument parsing, base64, time, Ctrl-C and libc/kernel32 FFI were deleted and
   replaced by serde/serde_json, clap (derive), anyhow, base64, chrono, ctrlc,
   percent-encoding, libc and windows-sys.
   - `src/pyjson.rs` is a small serde_json Formatter that prints Python `json.dumps`
     style. It keeps `labconf json`, the outputs summary and lab.json byte-identical to
     the Python output, and pure ASCII.
6. **Verified**, after the crate switch unless marked:
   - `cargo test`: 29 unit tests, 13 labconf tests and 13 fake-QEMU scenarios, all green.
   - `tests/parity.sh`: 73/73 checks against ../lib/*.py.
   - `cargo check` passes for x86_64-pc-windows-gnu and x86_64-apple-darwin.
   - Before the switch, a real KVM run, `wtl-rs lab run ../examples/configs/2019-smoke.conf`:
     exit 0, ALL STEPS OK (labs/state/2019/runs/20261022-214144). After it,
     `wtl-rs lab restore 2019` worked, and `wtl-rs lab status 2019` printed the same as
     `lab.sh status 2019`.
   - After the switch, the real KVM run of 2019-smoke passed again: exit 0, ALL STEPS OK
     (labs/state/2019/runs/20261022-222742, "RESULT OK", a 1024x768 screen.png). Then
     `restore` left no VMs or pid files, and `status` matched lab.sh again.

Nothing is committed. Another session may be editing lab.sh in the same checkout. Only
files under wtl-rs/ belong to this work.

## Multi-machine JSON labs (2026-10-23, in progress)

Goal: N-machine labs described by typed JSON (`labs/<name>.json` = the lab, `<app>.json` = a
run), alongside the existing 2-machine `.conf` path. Host: <=3 VMs at once (1 DC + 2 clients).

- **Part 1 done**: `src/labdef.rs` has the typed serde structs for both files
  (`deny_unknown_fields`), validation with clear errors before boot (unknown machine/user
  names, not exactly one DC, duplicate IPs, reserved/duplicate mount letters, bad script
  extensions, `as`-defaults-to-logon on a machine with no logon user), and defaults
  (role=member, DC .10 / members .20,.21,... in order, mem 4096/DC 3072, cpus 2).
  `LabDef`/`RunDef` are the raw shapes; `resolve()` returns `Lab`/`Run` for the runner.
  `src/schema.rs` + `wtl-rs schema [DIR]` write the JSON Schema (schemars, draft-07) to
  `wtl-rs/schema/{lab,run}.schema.json`. 9 unit tests. Decision: **two front-ends sharing one
  set of plumbing** — `.conf` keeps the old `labconf`+`lab.rs` dc/client path byte-for-byte
  (all 13 fake_lab + 13 labconf tests pass unchanged); JSON routes to a new N-machine runner
  (next). Verified multicast LAN (`-netdev dgram` one group) joins 3 VMs with no root on this
  host (target/tmp/mcast-test.sh).
- New crates: `indexmap` (machines/users keep JSON order for IP defaults), `schemars`.
- **Part 2 done**: `labdef::lab_from_conf` (bash vars from a `labs/*.conf` -> `LabDef`: DC +
  one member logged on as LAB_USER, LAB_ADMIN_USER -> a Domain Admins user) and
  `labdef::run_from_conf` (old `labconf::RunConfig` -> `RunDef`: `run_as_admin` -> `as system`,
  `run_as_student` -> the member's logon user, every step `on all`). Both feed the same
  `resolve()`. 2 unit tests. The live `.conf` run still uses labconf + lab.rs unchanged.

## Next

- The user tries it and switches `~/.local/bin/lab` to target/release/wtl-rs. Until then the
  bash `lab` stays the default.
- Optional, **ask the user first**: port build-image.sh, launch-test.sh, lib/isoinfo.py.
  `lab` still shells out to build-image.sh when a base image is missing.
- Never run on a real Windows or macOS host; only cross-`cargo check`ed. On real WHPX,
  `-accel whpx,kernel-irqchip=off -cpu max` may need `WTL_CPU`.
- If lab.sh gets bug fixes (`git log -- lab.sh lib/common.sh`), port them. src/lab.rs keeps
  lab.sh's function names (stop_vms, restore, freeze_off, make_cd, start_vm, boot_lab,
  reset_if_off, wait_ev, wait_logon, build_bases, prepare).

## Decisions

- **Crates for the generic parts, in-tree code for the win-test-lab logic** (ISO writer,
  QMP client, serial protocol, lab runner, bash-subset parser for labs/*.conf).
- **Errors** are anyhow throughout:
  - `labconf::ConfError` keeps the exact labconf.py texts.
  - `qmp::ReplyError` marks "QEMU answered with an error" (exit 1 in `wtl-rs qmp`); any
    other error there is a connect/protocol failure (exit 2).
  - `lab.rs` ends with an internal `Exit(code)` error once it has printed the reason
    itself: 2 lab error, 3 timeout.
- **clap usage errors** exit 2 under `lab` (like lab.sh) and 64 for the tools (like the
  Python scripts). Help and --version exit 0.
- **lib + thin bin** (`src/lib.rs`, `src/main.rs`), so the tests call the modules directly.
- **The state layout is unchanged**, so bash `lab` and `wtl-rs lab` work on the same labs.
  The only new file is `<vm>.qmp.addr`, written when QMP runs over TCP (the Windows default,
  or `WTL_QMP=tcp`).
- **labs/*.conf are read by a bash-subset parser** (src/shconf.rs). Anything that runs a
  command is an error with its line number.
- **Serial offsets are kept in memory** per VM, not in .off-* files. A half-written line is
  re-read on the next wait.
- **Deliberate differences**:
  - clap's wording for usage errors.
  - A non-integer `wait`/`timeout_min` is a clear error (Python crashed).
  - `last` is a text file where symlinks are not allowed.
  - `WTL_ACCEL`, `WTL_CPU`, `WTL_QMP`, `WTL_DISPLAY`, `WTL_ROOT` are new.
- `WTL_TEST_FAST=1` skips the fixed settle sleeps (1 s / 2 s / 20 s). Only the fake-QEMU
  tests use it.

## Gotchas

- **The Bash tool's sandbox is broken on this machine.** Run commands with the Terminal
  panel tools (run_in_terminal / read_terminal).
  - `$TMPDIR` is empty in that shell, so put scratch output in `target/tmp/`.
  - The terminal hook rejects any command line containing `!` (so no `sed` that writes
    `bail!`). Edit Rust macros with the Edit tool instead.
- **The Write/Edit tools turned a literal `é` in Rust source into `é`.** JSON tests build
  their expected escapes with `format!` to avoid that.
- **tests/fake_lab.rs is `harness = false`**: one binary that is the test driver, the fake
  qemu-system-x86_64 (argv contains `-machine`) and the fake qemu-img (argv[1] = `create`).
  To run some scenarios: `cargo test --test fake_lab -- <substring of the scenario name>`.
  The temp dir is kept on failure.
- **A fake VM must not exit within 0.5 s of starting.** `qemu::start` treats that as "qemu
  failed to start".
- **mkiso.py quirks to keep**: file stems are cut to `30 - 1 - len(ext)` chars, Joliet
  collision renames are checked case-insensitively, and files are laid out in ISO-tree BFS
  order.
- **serde_json needs `preserve_order`** so the outputs summary keeps Python's key order.
- **Real labs**: 2019 has a 0 state with the SYSTEM worker. 2012/2016 0 states predate the
  worker and need `lab rebuild` before config runs work.

## Log

- 2026-10-22 21:41: first real KVM run (zero-dep version): pass.
- 2026-10-22 22:27: crate version, real KVM run of 2019-smoke: pass; lab restored to its 0 state.
