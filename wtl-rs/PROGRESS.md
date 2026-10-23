# wtl-rs progress

Hand-over notes: the current state, decisions and gotchas. User docs: ../README.md,
../docs/CONFIG.md; design: ../SPEC.md; contributor notes: ../docs/AGENTS.md.

## State (2026-10-23)

- The host side is Rust only. `lab` = `wtl-rs` (`~/.local/bin/lab` -> target/release/wtl-rs).
  lab.sh, lib/labconf.py, tests/test_labconf.py and lib/msdl.py are gone.
- JSON is the one format: `labs/*.json` (school, 2012, 2016, 2019, win10, win11) and
  `examples/configs/*.json`. The `.conf` runner, its parsers (labconf.rs, shconf.rs) and the
  `.conf` -> JSON importers were removed; so was the two-machine `-netdev stream` LAN.
- Labs: one DC + N members on a `-netdev dgram` multicast LAN; computer name = machine key
  (the DC renames before promotion, members via `Add-Computer -NewName`); missing groups are
  created; missing base images are installed with build-image.sh; every run writes
  `runs/<id>/result.json`.
- Verified on KVM: the school lab (2019 DC + 2012 + 2016) built from nothing (base images,
  0 state) and its smoke run passes (ALL STEPS OK, 6 steps on 3 machines).
- No-VM checks: cargo test (unit + 15 fake_lab scenarios), clippy clean, fmt, rustdoc with
  `-D warnings`, cross-checks for Windows / macOS, tests/parity.sh, tests/all.sh.

## Lab modes (branch lab-modes, 2026-10-23)

- AD mode (one `role: "dc"`, any Server version) or workgroup mode (no DC: local users, no
  domain; one machine needs no `network`). A 2012 DC + 2019 member server works
  (`labs/mixed.json`). Run files name a lab by name or by path (`"./lab.json"`); a lab file
  outside `labs/` gets the state name `<stem>-<fnv hash of its path>` unless it sets `"name"`.
- Steps "as" another user use LogonUser + CreateProcessAsUser in labsvc.ps1 (local `.\user`
  or `DOMAIN\user`); every machine with steps gets all of the run's mounts.
- Passwords containing the user name are rejected before boot.
- Verified on KVM: single-2019 (workgroup), mixed (2012 DC + 2019 SRV1, steps as SYSTEM, as
  LAB\operator and as LAB\appuser), and a project folder outside the repo with its own
  wintest/lab.json (install.ps1 + smoke.ps1, steps as SYSTEM / appuser / ops).
- Docs: docs/USING.md (from your own project, CI, limits), CONFIG/SPEC/AGENTS updated.

## Server roles (branch lab-roles, 2026-10-23)

- `roles` (`ad-ds`, `ad-cs`, `dhcp`, `iis`; `role: "dc"` = `["ad-ds"]`), `features`, `setup`
  scripts and `ad_cs`/`dhcp`/`iis` options per machine, applied in a build-only `configure`
  phase and frozen into the 0 state. Validated before boot (one ad-ds, ad-cs needs AD,
  server OS only, role xor roles).
- Verified on KVM: `labs/pki.json` (2019 DC + DHCP, 2012 R2 CA + IIS, 2016 client) built from
  base images; `examples/configs/pki-smoke.json` passes (computer + user cert from
  PKI-ROOT-CA on CL1, http://CA1/ 200, DHCP scope on DC1).
- Follow-ups: a standalone (workgroup) CA; more roles (e.g. `dns`, `file-server`).

## Next (ask the user first)

- A `targets` matrix in one run file (run the same steps on several labs).

- Port build-image.sh / launch-test.sh / lib/isoinfo.py to Rust, so installing base images
  needs no bash or Python (then Windows/macOS hosts need nothing but QEMU).
- Never run on a real Windows or macOS host; only cross-`cargo check`ed. On WHPX,
  `-accel whpx,kernel-irqchip=off -cpu max` may need `WTL_CPU`.

## Decisions

- Crates for the generic parts, in-tree code for the win-test-lab logic (ISO writer, QMP,
  serial protocol, runner).
- Steps are guest-driven: each machine gets its ordered step list on its control disc; there
  is no barrier between different machines' steps (the host has no write channel into a
  booted guest; mounts are read-only ISOs).
- Errors are anyhow; the runner ends with an internal `Exit(code)` once it printed the reason:
  2 lab error, 3 timeout. clap usage errors exit 2 under `lab`, 64 for the other tools.
- Serial offsets are kept in memory per VM; a half-written line is re-read by the next wait.
- `pyjson` writes Python-`json.dumps`-style ASCII JSON: lab.json must be ASCII for
  PowerShell 4, and the outputs summary is compared byte for byte with lib/outputs.py.
- `WTL_TEST_FAST=1` skips the fixed settle sleeps (fake-QEMU tests only).

## Gotchas

- The Bash tool's sandbox is broken on this machine: run commands with the Terminal panel
  tools. The terminal hook rejects lines with `!`, a leading `#` word, or `(...)` glob-like
  groups, and multi-line commands; put scripts in `target/tmp/` and run them.
- A fake VM must not exit within 0.5 s of starting (`qemu::start` treats that as a failure).
- rustfmt.toml keeps the long-line style (max_width 140).
