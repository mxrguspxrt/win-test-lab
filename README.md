# win-test-lab

Test your Windows software inside a small Active Directory domain, on any mix of Windows
versions, on your own Linux machine: a domain controller plus domain-joined machines, each
logged on as an ordinary domain user, all on a private network. Describe the lab once in a
JSON file, run your program on it, look at the results, reset it to a clean state in a
second, repeat.

```
lab run examples/configs/school-smoke.json
```

boots the example lab `school` (a Server 2019 DC, a 2012 R2 and a 2016 member), mounts a
folder from your machine as drive `X:` on the machines, runs the steps from the config on
each machine as the user it names, and prints every step's exit code.

It runs on QEMU/KVM with no root, no Samba and no internet inside the lab. Windows comes from
Microsoft's free evaluation ISOs.

## Start here

**1. Set up the host (once).**

    ./setup-host.sh                  # installs qemu + OVMF, checks /dev/kvm
    # put the Windows evaluation ISOs in iso-files/  (see docs/ISOS.md; ./download-isos.sh 2019 tries to fetch one)

**2. Build `lab` (once, and after pulling changes).** Needs Rust (https://rustup.rs).

    cd wtl-rs && cargo build --release && cd ..
    ln -sfn "$PWD/wtl-rs/target/release/wtl-rs" ~/.local/bin/lab

The binary acts as `lab` when called by that name. It finds this folder from its own location.

**3. Run the example.**

    examples/smoke-test/build.sh                   # builds smoke.exe (needs gcc-mingw-w64-x86-64)
    lab run examples/configs/school-smoke.json

The first run installs Windows for each base image it needs (unattended, ~10-30 min, in
parallel) and builds the lab's 0 state: the domain, the users, the joined members (~20 min).
Every later run starts from that 0 state and takes a couple of minutes.

**4. Look, then reset.**

    lab status school                # the machines, their VNC addresses, the last run
    lab restore school               # back to the 0 state (VMs off), about a second

The VMs stay up after a run; connect with any VNC viewer to the printed `127.0.0.1:59xx`.
Results are in `labs/state/school/runs/<time>/`: `result.json` (every step with machine,
user, exit code, duration), each step's output, a screenshot per machine.

## Your own lab and program

- A **lab** (`labs/<name>.json`) names the domain, the machines (computer name, Windows
  version, which user logs on) and the users and groups.
- A **run config** names the lab, the host folders to mount as drives, and the steps: what to
  run, on which machine, as `system` or as which user.

Both formats, every field and its default: [docs/CONFIG.md](docs/CONFIG.md). JSON Schemas
for editor autocompletion are in [wtl-rs/schema/](wtl-rs/schema/).

## Commands

    lab run CONFIG      restore, boot, mount, run the steps, report (exit 0 = all steps passed)
    lab up LAB          boot the lab from its 0 state and leave it running
    lab status LAB      0 state, machines (running/off, VNC), last run
    lab restore LAB     stop and throw away everything since the 0 state
    lab stop LAB        power off, keeping changes until the next run
    lab prepare LAB     build the 0 state now
    lab rebuild LAB     delete the 0 state; the next run builds a new domain

`--show` opens the VM windows instead of VNC. Exit codes: 0 ok, 1 a step failed, 2 lab
error, 3 timeout.

## Platforms

| host | accelerator | status |
|------|-------------|--------|
| Linux | KVM | Supported and tested. `/dev/kvm` must be writable (`./setup-host.sh`). |
| Windows | WHPX | Compiles; not tested on real hardware. Needs QEMU for Windows and "Windows Hypervisor Platform" enabled; QMP uses a loopback TCP port. Installing base images needs bash (`build-image.sh`). |
| macOS, Intel | HVF | Compiles; not tested on real hardware. |
| macOS, Apple Silicon | none | x86 Windows can only be emulated: far too slow. Not supported. |

Settings are environment variables (`WTL_QEMU`, `WTL_QEMU_IMG`, `WTL_ISO_DIR`,
`WTL_IMAGES_DIR`, `WTL_ACCEL`, `WTL_CPU`, `WTL_QMP`, `WTL_OVMF_CODE`, `WTL_OVMF_VARS`, ...);
see [SPEC.md](SPEC.md).

## More

- [docs/CONFIG.md](docs/CONFIG.md): the lab and run formats, what a run produces.
- [docs/ISOS.md](docs/ISOS.md): getting the Windows ISOs.
- [SPEC.md](SPEC.md): how it works inside (host, guest scripts, serial protocol, network).
- [docs/AGENTS.md](docs/AGENTS.md): orientation for contributors and coding agents.
- `launch-test.sh`: a separate, older single-VM tester (one machine, no domain:
  `./launch-test.sh program.zip program.exe 2019 --test check.py`), described in SPEC.md.

## Tests (no VM needed)

    tests/all.sh

runs `cargo test` (unit tests and a 15-scenario end-to-end suite against a fake QEMU), the
PowerShell 4 lint of the guest scripts, the lab and config checks, and the fake-QEMU suite of
`build-image.sh` / `launch-test.sh`.

License: MIT.
