# win-test-lab

**Automated Windows installs and tests, on many Windows versions, from a JSON file.**

Ever tested your software on several Windows versions by hand, in VirtualBox? Something always
gets forgotten, and then you spend half a day debugging the test setup instead of your code.
win-test-lab does the boring part for you:

- **Install Windows unattended** from Microsoft's ISOs: Server 2012 R2 to 2025, Windows 10, Windows 11
  (x64 and Arm64). You download the ISOs; everything after that is automatic.
- **A clean state, every time.** Each lab is built once into a "0 state". Every run starts
  from it, and `lab restore` takes you back in a second.
- **Real networks, too.** One machine, or a whole Active Directory domain: a domain controller,
  DHCP, a certificate authority (AD CS) with auto-enrollment, IIS, members and users.
- **Run your program** inside Windows, as SYSTEM or as any lab user, and get every step's exit
  code and output back.

## What it looks like

One Windows 11 machine, your build folder mounted as `X:`, two steps:

```json
{
  "lab": "single-win11",
  "mount": { "X": "../smoke-test/out" },
  "steps": [
    { "as": "system", "run": "X:/setup.bat" },
    { "run": "X:/smoke.exe" }
  ]
}
```

```
lab run examples/configs/single-win11-smoke.json
```

An Active Directory lab: a 2019 DC with DHCP, a 2012 R2 server with an Enterprise CA and IIS,
and a 2016 member that a domain user logs on to:

```json
{
  "domain": "pki.local",
  "netbios": "PKI",
  "network": { "subnet": "10.77.60", "lan_port": 47060 },
  "machines": {
    "DC1": { "os": "2019", "roles": ["ad-ds", "dhcp"], "dhcp": { "range": ".100-.199" } },
    "CA1": { "os": "2012", "roles": ["ad-cs", "iis"], "ad_cs": { "ca_name": "PKI-ROOT-CA", "autoenroll": true } },
    "CL1": { "os": "2016", "logon": "student" }
  },
  "users": {
    "student": { "password": "Pupil!2026" }
  }
}
```

and a run on it, each step on the machine and as the user it names:

```json
{
  "lab": "pki",
  "mount": { "X": "../pki" },
  "steps": [
    { "on": "DC1", "as": "system",  "run": "X:/dhcp-scope.ps1" },
    { "on": "CL1", "as": "system",  "run": "X:/computer-cert.ps1" },
    { "on": "CL1", "as": "student", "run": "X:/user-cert.ps1" },
    { "on": "CL1", "as": "student", "run": "X:/web.ps1" }
  ]
}
```

That is the whole interface: a **lab** file (the machines) and a **run** file (what to run on
them). Both have JSON Schemas ([wtl-rs/schema/](wtl-rs/schema/)), so your editor completes and
checks them.

**Why:** with a coding agent such as Claude Code, this becomes a loop the agent can run on its
own: build your code, `lab run` it on every Windows you care about, read the results, fix the
bug, run again.

## Start

You need QEMU (`brew install qemu` on macOS; `./setup-host.sh` on Debian/Ubuntu) and Rust
(https://rustup.rs).

```
cd wtl-rs && cargo build --release && cd ..
ln -sfn "$PWD/wtl-rs/target/release/wtl-rs" ~/.local/bin/lab
lab doctor                    # what this computer can run, which ISOs you have
```

Download the Windows ISOs you need from Microsoft into `iso-files/` (links and which OS has
which CPU: [docs/ISOS.md](docs/ISOS.md)), then:

```
lab check-iso                                           # what each ISO is, which lab key it serves
lab run examples/configs/single-win11-smoke.json        # one machine
lab run examples/configs/pki-smoke.json                 # an AD domain with a CA
```

The first run of a lab installs Windows (unattended, 10-50 min per OS) and builds its 0 state.
Later runs start from that state and take a few minutes. Then:

```
lab status pki        # machines, VNC addresses, the last run
lab restore pki       # back to the 0 state, about a second
```

Results land in `labs/state/<lab>/runs/<time>/`: `result.json` (every step: machine, user,
exit code, duration), each step's output, and a screenshot of each machine.

## On a Mac with Apple Silicon: Windows on Arm

Windows 11 on Arm runs natively on an M-series Mac (Apple's hypervisor, no emulation), and it
runs **both Arm64 and x64 programs**: Windows translates an x64 exe on the fly, per program,
with nothing to set up.

```
lab run examples/configs/single-win11-arm64-smoke.json
```

runs the same tiny program twice, built for Arm64 and for x64 ([examples/smoke-rs](examples/smoke-rs/),
built from any OS with only rustup):

```json
{
  "lab": "single-win11-arm64",
  "mount": { "X": "../smoke-rs/out" },
  "steps": [
    { "as": "system", "run": "X:/setup.bat" },
    { "run": "X:/smoke-arm64.exe" },
    { "run": "X:/smoke-x64.exe" }
  ]
}
```

Real output of that run (`labs/state/single-win11-arm64/runs/<time>/WSARM64/step*out.txt`):

```
smoke.exe running as WSARM64\tester
exe built for arm64, machine CPU arm64
RESULT OK

smoke.exe running as WSARM64\tester
exe built for x64, machine CPU arm64 (running translated)
RESULT OK
```

Measured on an M-series MacBook Air (4 vCPUs, 4 GB per VM):

| step | time |
|------|------|
| install Windows 11 Arm64 into a base image (once) | 48 min |
| build the lab's 0 state: rename, local user, autologon (once per lab) | 2 min |
| `lab run`: boot from the 0 state, run 3 steps, report | 3 min |
| `lab restore` | about a second |

What you need for it:

- the Windows 11 Arm64 ISO from Microsoft (the only Windows with a free Arm64 ISO; there is no
  Windows Server for Arm);
- two drivers from virtio-win (network + the report channel; Windows on Arm ships no driver for
  any network card QEMU can emulate): see [docs/ISOS.md](docs/ISOS.md#drivers-for-arm64-machines-virtio-win).

x64 Windows (every Server) also runs on an Arm Mac, but only emulated (measured: a Server 2025
install takes 73 min and PowerShell needs ~50 s just to start), so it is not practical for
daily testing. For x64 Windows use a Linux machine with KVM (or an Intel Mac).

| host | x64 Windows (Servers, Win10, Win11) | Windows 11 on Arm |
|------|-------------------------------------|-------------------|
| Linux x64 | native (KVM). Supported and tested. | emulated |
| macOS, Apple Silicon | emulated (slow) | native (HVF). Tested. |
| macOS, Intel | native (HVF). Compiles, not tested. | emulated |
| Windows | native (WHPX). Compiles, not tested. | emulated |

## Commands

    lab run CONFIG          restore, boot, mount, run the steps, report (exit 0 = all steps passed)
    lab up LAB              boot the lab from its 0 state and leave it running
    lab status LAB          0 state, machines (running/off, VNC), last run
    lab restore LAB         stop and throw away everything since the 0 state
    lab stop LAB            power off, keeping changes until the next run
    lab prepare LAB         build the 0 state now
    lab rebuild LAB         delete the 0 state; the next run builds it again
    lab doctor              what this host can run: QEMU per CPU, acceleration, firmware, ISOs
    lab check-iso [FILE]    an ISO's Windows build, CPU and editions (--sha256 HASH to verify it)

Exit codes: 0 ok, 1 a step failed, 2 lab error, 3 timeout. `--show` opens the VM windows
instead of VNC.

Ready-made labs in [labs/](labs/): one machine each (`single-2012`, `single-2016`,
`single-2019`, `single-2025`, `single-win10`, `single-win11`, `single-win11-arm64`), AD labs
(`2012`, `2016`, `2019`, `win10`, `win11`, `mixed`, `school`, `pki`). Run
examples: [examples/configs/](examples/configs/).

## More

- [docs/USING.md](docs/USING.md): using win-test-lab from your own project, CI.
- [docs/CONFIG.md](docs/CONFIG.md): every field of the lab and run files, what a run produces.
- [docs/ISOS.md](docs/ISOS.md): getting and checking the Windows ISOs; the Arm64 drivers.
- [SPEC.md](SPEC.md): how it works inside (QEMU, guest scripts, the report channel, settings).
- [docs/AGENTS.md](docs/AGENTS.md): orientation for contributors and coding agents.

Tests that need no VM: `tests/all.sh`. License: MIT.
