# wtl-rs: the win-test-lab host program

`wtl-rs` is the host side of win-test-lab. Called as `lab` (a symlink or a copy with that
name) it is the lab runner; see [../README.md](../README.md) for using it and
[../docs/CONFIG.md](../docs/CONFIG.md) for the lab and run formats. How it fits together:
[../SPEC.md](../SPEC.md).

    cargo build --release
    ln -sfn "$PWD/target/release/wtl-rs" ~/.local/bin/lab

Subcommands besides `lab`:

    wtl-rs schema [DIR]                     write schema/lab.schema.json + run.schema.json
    wtl-rs mkiso -o OUT.iso -V VOLID SRC_DIR
    wtl-rs qmp SOCK status|powerdown|quit|screendump OUT.png|raw JSON   (SOCK: path or tcp:HOST:PORT)
    wtl-rs serialwait LOG --until EV[,EV] [--fail ..] [--timeout S] [--pid PID] ...
    wtl-rs outputs SERIAL_LOG RESULTS_DIR

`mkiso`, `serialwait` and `outputs` behave like `lib/mkiso.py`, `lib/serialwait.py` and
`lib/outputs.py`, which `build-image.sh` and `launch-test.sh` still use; `tests/parity.sh`
checks that mkiso and outputs agree with them.

## Modules

| module | what |
|--------|------|
| `lab` | the `lab` CLI; finding the checkout; installing base images with build-image.sh |
| `labdef` | the JSON formats: raw serde types, validation, defaults, JSON Schema |
| `labrun` | the runner: 0 state, boot, control/mount discs, steps, `result.json` |
| `qemu` | `WTL_*` settings, the QEMU command line, starting QEMU |
| `qmp` | QEMU monitor client (TCP or unix socket), PPM -> PNG screenshots |
| `serialwait` | wait for `WTL` events in a growing serial log |
| `outputs` | decode program output sent over serial |
| `mkiso` | ISO9660 + Joliet writer (and a reader for the tests) |
| `oslist` | the OS table `lib/oslist.conf` |
| `sys` | process liveness / stop, detached spawn, Ctrl-C |
| `util`, `logx`, `pyjson`, `schema` | small helpers, log lines, Python-style JSON, `wtl-rs schema` |

`cargo doc --no-deps --open` has the details.

## Tests

    cargo test                  # unit tests + tests/fake_lab.rs (15 end-to-end scenarios)
    tests/parity.sh             # mkiso / outputs vs lib/*.py (needs python3)
    cargo clippy --all-targets && cargo fmt --check
    cargo check --target x86_64-pc-windows-gnu    # the Windows / macOS code paths compile
    cargo check --target x86_64-apple-darwin      # (rustup target add ... first)

`tests/fake_lab.rs` is `harness = false`: one binary that is the test driver, a fake
`qemu-system-x86_64` (it serves QMP and plays `guest/lab.ps1` from the control disc's
`lab.json` and the mount discs) and a fake `qemu-img`. Run some scenarios with
`cargo test --test fake_lab -- <part of a scenario name>`; the temp dir is kept on failure.
