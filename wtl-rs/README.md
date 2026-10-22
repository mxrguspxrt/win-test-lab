# wtl-rs: the win-test-lab host side in Rust

A single program that replaces the host side of win-test-lab (`lab.sh`, `lib/common.sh` and
`lib/{labconf,mkiso,qmp,serialwait,outputs}.py`), so a lab can eventually be driven from
Linux, Windows or macOS. The guest side (`guest/*.ps1`) and the serial protocol in
[../SPEC.md](../SPEC.md) are unchanged. Your existing files keep working as they are:

- run configs (`examples/configs/*.conf`) are parsed with the same keys and rules, give the
  same error messages, and keep their steps in file order;
- lab definitions (`labs/*.conf`, bash `KEY=value` files) are read as bash would read them
  (quotes, `#` comments, `$VAR` / `${VAR:-default}`). A line that would run a command is
  rejected with its line number;
- the state layout (`labs/state/<lab>/`, the frozen `*-0.qcow2` 0 state, `runs/<time>/`) is
  the same, so the bash `lab` and `wtl-rs lab` work on the same labs.

The bash `lab` stays the default until you have tried this one.

## Build and run

    cd win-test-lab/wtl-rs
    cargo build --release
    ./target/release/wtl-rs lab run ../examples/configs/2019-smoke.conf

Dependencies are a few small, well-known crates: serde/serde_json, clap, anyhow, base64,
chrono (local time), ctrlc, percent-encoding, and libc / windows-sys for process control.
The ISO writer, QMP client, serial protocol and lab runner are in this crate.

The commands and options are the same as for `lab.sh`:

    wtl-rs lab run CONFIG [CONFIG...] [--show] [--wait S] [--timeout-min M]
    wtl-rs lab restore|up|stop|status|prepare|rebuild LAB_OR_CONFIG
    wtl-rs lab help-config

`--help` works on every level (`wtl-rs lab run --help`).

If the binary is named `lab` (a symlink or a copy), it behaves as `wtl-rs lab`. To switch
over later:

    ln -sfn "$PWD/target/release/wtl-rs" ~/.local/bin/lab

`wtl-rs` finds the win-test-lab folder from its own location. Set `WTL_ROOT` if you copy
the binary somewhere else.

The Python tools are subcommands too, with the same arguments and exit codes:

    wtl-rs labconf json|check CONFIG...
    wtl-rs mkiso -o OUT.iso -V VOLID SRC_DIR
    wtl-rs qmp SOCK status|powerdown|quit|screendump OUT.png|raw JSON   (SOCK: path or tcp:HOST:PORT)
    wtl-rs serialwait LOG --until EV[,EV] [--fail ..] [--timeout S] [--pid PID] [--from-offset N] ...
    wtl-rs outputs SERIAL_LOG RESULTS_DIR

## Settings

The same environment variables as `lib/common.sh`: `WTL_QEMU`, `WTL_QEMU_IMG`,
`WTL_IMAGES_DIR`, `WTL_ISO_DIR`, `WTL_NO_KVM=1`, `WTL_CPUS`, `WTL_PASSWORD`, `WTL_NIC`,
`WTL_OVMF_CODE`, `WTL_OVMF_VARS`. You can also set them in a lab definition. New ones:

| variable       | meaning                                                                   |
|----------------|---------------------------------------------------------------------------|
| `WTL_ROOT`     | the win-test-lab folder (default: found from the binary's location)       |
| `WTL_ACCEL`    | `kvm`, `whpx`, `hvf` or `tcg` (default: the host's; `WTL_NO_KVM=1` = tcg)  |
| `WTL_CPU`      | the `-cpu` value (default: `host,hv_*` on KVM, `host` on HVF, `max` otherwise) |
| `WTL_QMP`      | `unix` (default on Linux/macOS) or `tcp` (default on Windows)             |
| `WTL_DISPLAY`  | the display `--show` uses (default `gtk`; `cocoa` on macOS)               |

## Platforms

| host | accelerator | notes |
|------|-------------|-------|
| **Linux** | KVM (`-enable-kvm -cpu host,hv_*`) | The supported host and the one this was tested on. `/dev/kvm` must be writable (`./setup-host.sh`). |
| **Windows** | WHPX (`-accel whpx,kernel-irqchip=off`) | Enable "Windows Hypervisor Platform" in Windows Features and install QEMU for Windows (`qemu-system-x86_64.exe`, `qemu-img.exe` on `PATH`, or `WTL_QEMU` / `WTL_QEMU_IMG`). The QEMU monitor (QMP) uses a loopback **TCP** port, because Windows has no unix sockets for it. The port is saved in `labs/state/<lab>/<vm>.qmp.addr`, so `stop` / `restore` / `status` find it again. WHPX is slower than KVM and some QEMU builds need `WTL_CPU` set to a named model (e.g. `Skylake-Client`). Point `WTL_OVMF_CODE` / `WTL_OVMF_VARS` at QEMU's `edk2-x86_64-code.fd` / `edk2-i386-vars.fd` for the UEFI (Win11) lab. Building base images still needs `build-image.sh` (bash), which is not ported yet. |
| **macOS, Intel** | HVF (`-accel hvf -cpu host`) | Install QEMU (e.g. Homebrew). Unix-socket QMP works as on Linux. |
| **macOS, Apple Silicon** | none usable | x86 Windows can only be emulated (TCG). That is far too slow for Windows Server: booting and promoting a DC takes hours. This is not a supported setup. |

Common to all: the lab LAN between the two VMs is QEMU's `-netdev stream` on
127.0.0.1:`LAB_LAN_PORT`, and mounted folders are read-only ISOs written by the built-in
`mkiso`. Neither needs root, Samba, or anything else installed.

## Tests

    cargo test              # unit tests, the run-config spec, and an end-to-end lab test
    tests/parity.sh         # compare with the Python originals in ../lib (needs python3)
    cargo check --target x86_64-pc-windows-gnu    # the Windows / macOS code paths compile
    cargo check --target x86_64-apple-darwin      # (rustup target add ... first)

- `tests/labconf.rs` is `tests/test_labconf.py` ported case by case (the spec for the
  parser), plus the exact error texts, edge cases, the JSON form, and every example config.
- `tests/fake_lab.rs` runs the real `wtl-rs lab` against a fake QEMU and qemu-img (the same
  test binary). The fake serves QMP and plays the guest from the control disc's `lab.json`
  and the mount discs. It covers: building the 0 state, running config steps in order,
  failing steps (exit 1), `LAB_FAIL` (exit 2 + screenshot, including the old-QEMU PPM path),
  timeouts (exit 3), a VM that dies, `up` / `stop` / `restore` / `rebuild` / `prepare` /
  `status`, layered configs, QMP over TCP, the UEFI client, and being called as `lab`.
- `tests/parity.sh` checks the run-config parser (JSON and error output on about 25 valid
  and broken configs), mkiso (images identical byte for byte apart from timestamps) and
  outputs (on the serial logs in `labs/state`) against the Python originals.

## Differences from lab.sh

All on purpose; nothing in the config or lab file formats changed:

- Command-line mistakes (unknown command or option, bad `--wait`) are reported in clap's
  words (`error: unexpected argument '--bogus' found`). The exit code is still 2.
- `lab status` shows disk sizes in the same `du -h` style, computed by the program itself.
- A config's `wait` / `timeout_min` must be whole numbers. `labconf.py` crashed with a
  Python traceback on anything else; this reports `CONFIG: wait 'x' must be a whole number`.
- QEMU is started in its own session (as with `setsid`), and Ctrl-C leaves the VMs running
  with the same "interrupted; VMs may still be running" message.
- Where `labs/state/<lab>/last` cannot be a symlink (Windows without developer mode), it is
  a small text file naming the run folder.
- Not ported yet: `build-image.sh` (installing Windows into a base image), `launch-test.sh`
  (the single-VM tester) and `lib/isoinfo.py`. `wtl-rs lab` still calls `build-image.sh`
  when a base image is missing.
