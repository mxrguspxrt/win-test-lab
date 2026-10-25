# win-test-lab

**Automated Windows installs and tests, on many Windows versions, from a JSON file.**

Ever tested your software on several Windows versions by hand, in VirtualBox? Something always
gets forgotten, and then you spend half a day debugging the test setup instead of your code.
win-test-lab (the `wtl` command) does the boring part for you:

- **Install Windows unattended** from Microsoft's ISOs: Server 2012 R2 to 2025, Windows 10, Windows 11
  (x64 and Arm64). You download the ISOs; everything after that is automatic.
- **A clean state, every time.** Each lab is built once into a "0 state". Every run starts
  from it, and `wtl restore` takes you back in a second.
- **Real networks, too.** One machine, or a whole Active Directory domain: a domain controller,
  DHCP, a certificate authority (AD CS) with auto-enrollment, IIS, members and users.
- **Run your program** inside Windows, as SYSTEM or as any lab user, and get every step's exit
  code and output back.

## What it looks like

One Windows 11 machine, your build folder mounted as `X:`, two steps:

```json
{
  "lab": "single-win11",
  "mount": { "X": "./build" },
  "steps": [
    { "as": "system", "run": "X:/setup.bat" },
    { "run": "X:/smoke.exe" }
  ]
}
```

```
wtl run smoke.json
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
  "mount": { "X": "./pki" },
  "steps": [
    { "on": "DC1", "as": "system",  "run": "X:/dhcp-scope.ps1" },
    { "on": "CL1", "as": "system",  "run": "X:/computer-cert.ps1" },
    { "on": "CL1", "as": "student", "run": "X:/user-cert.ps1" },
    { "on": "CL1", "as": "student", "run": "X:/web.ps1" }
  ]
}
```

That is the whole interface: a **lab** file (the machines) and a **run** file (what to run on
them). Both have JSON Schemas (`wtl schema`, or
[schema/](https://github.com/mxrguspxrt/win-test-lab/tree/main/schema)), so your editor
completes and checks them.

**Why:** with a coding agent, this becomes a loop the agent can run on its own: build your
code, `wtl run` it on every Windows you care about, read the results, fix the bug, run again.

## Install

You need QEMU and Rust (https://rustup.rs):

| host | QEMU |
|------|------|
| Debian / Ubuntu | `sudo apt install qemu-system-x86 qemu-utils ovmf` (and `sudo usermod -aG kvm $USER`) |
| Fedora | `sudo dnf install qemu-kvm qemu-img edk2-ovmf` |
| macOS | `brew install qemu` |
| Windows | `winget install SoftwareFreedomConservancy.QEMU` |

```
cargo install win-test-lab    # installs the `wtl` command
wtl doctor                    # what this computer can run, which ISOs you have
```

Or from a clone: `cargo build --release`, then use `target/release/wtl`.

## Start

Download the Windows ISOs you need from Microsoft into the ISO folder (`wtl home` shows where;
links and which OS has which CPU:
[docs/ISOS.md](https://github.com/mxrguspxrt/win-test-lab/blob/main/docs/ISOS.md)), then:

```
wtl check-iso                 # what each ISO is, which OS key it serves
wtl status single-win11       # a lab built into wtl: one Windows 11 machine
wtl run smoke.json            # a run file like the one above
```

The first run of a lab installs Windows (unattended, 10-50 min per OS) and builds its 0 state.
Later runs start from that state and take a few minutes. Then:

```
wtl status pki        # machines, VNC addresses, the last run
wtl restore pki       # back to the 0 state, about a second
```

Results land in the lab's state folder, `runs/<time>/`: `result.json` (every step: machine,
user, exit code, duration), each step's output, and a screenshot of each machine.

## Where the data lives

ISOs, base images (20-30 GB each) and 0 states live in one folder, the **home**. `wtl home`
shows which folder it is and why. The first match wins:

1. `--home DIR`, or the `WTL_HOME` variable
2. the current folder or a parent of it with a `wtl.json` (`wtl init` writes one), or a
   win-test-lab checkout
3. `home` in the user config file (`~/.config/win-test-lab/config.json`; macOS
   `~/Library/Application Support/win-test-lab/config.json`; Windows
   `%APPDATA%\win-test-lab\config\config.json`)
4. the per-user data folder: `~/.local/share/win-test-lab`, macOS
   `~/Library/Application Support/win-test-lab`, Windows `%LOCALAPPDATA%\win-test-lab\data`

Each folder can be moved on its own, e.g. the images to a bigger disk. The variable wins over
the home's `wtl.json`, which wins over the user config file:

| folder | variable | config key | default |
|--------|----------|------------|---------|
| Windows ISOs | `WTL_ISO_DIR` | `iso_dir` | `iso-files/` |
| base images | `WTL_IMAGES_DIR` | `images_dir` | `images/` |
| 0 states, runs, logs | `WTL_STATE_DIR` | `state_dir` | `labs/state/` |
| image build scratch | `WTL_RUNS_DIR` | `runs_dir` | `runs/` |
| arm64 drivers | `WTL_DRIVERS_DIR` | `drivers_dir` | `drivers/` |

```json
{ "images_dir": "/mnt/big/wtl-images", "iso_dir": "../isos" }
```

Relative paths in a config file are relative to that file. A lab name (`school`) is
`labs/school.json` in the home if there is one, else the lab of that name built into `wtl`.

## On a Mac with Apple Silicon: Windows on Arm

Windows 11 on Arm runs natively on an M-series Mac (Apple's hypervisor, no emulation), and it
runs **both Arm64 and x64 programs**: Windows translates an x64 exe on the fly, per program,
with nothing to set up.

```json
{
  "lab": "single-win11-arm64",
  "mount": { "X": "./out" },
  "steps": [
    { "as": "system", "run": "X:/setup.bat" },
    { "run": "X:/smoke-arm64.exe" },
    { "run": "X:/smoke-x64.exe" }
  ]
}
```

runs the same tiny program twice, built for Arm64 and for x64
([examples/smoke-rs](https://github.com/mxrguspxrt/win-test-lab/tree/main/examples/smoke-rs),
built from any OS with only rustup). Real output of that run:

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
| `wtl run`: boot from the 0 state, run 3 steps, report | 3 min |
| `wtl restore` | about a second |

What you need for it:

- the Windows 11 Arm64 ISO from Microsoft (the only Windows with a free Arm64 ISO; there is no
  Windows Server for Arm);
- two drivers from virtio-win (network + the report channel; Windows on Arm ships no driver for
  any network card QEMU can emulate): see
  [docs/ISOS.md](https://github.com/mxrguspxrt/win-test-lab/blob/main/docs/ISOS.md#drivers-for-arm64-machines-virtio-win).

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

    wtl run CONFIG          restore, boot, mount, run the steps, report (exit 0 = all steps passed)
    wtl up LAB              boot the lab from its 0 state and leave it running
    wtl status LAB          0 state, machines (running/off, VNC), last run
    wtl restore LAB         stop and throw away everything since the 0 state
    wtl stop LAB            power off, keeping changes until the next run
    wtl prepare LAB         build the 0 state now
    wtl rebuild LAB         delete the 0 state; the next run builds it again
    wtl doctor              what this host can run: QEMU per CPU, acceleration, firmware, ISOs
    wtl check-iso [FILE]    an ISO's Windows build, CPU and editions (--sha256 HASH to verify it)
    wtl home                where the data is, and where each setting comes from
    wtl init [DIR]          make DIR a home (writes DIR/wtl.json)
    wtl build-image OS      install one base image now (wtl run does it when one is missing)
    wtl schema [DIR]        write the JSON Schemas of the lab and run files

Exit codes: 0 ok, 1 a step failed, 2 lab error, 3 timeout. `--show` opens the VM windows
instead of VNC.

Labs built into `wtl`: one machine each (`single-2012`, `single-2016`, `single-2019`,
`single-2025`, `single-win10`, `single-win11`, `single-win11-arm64`), AD labs (`2012`, `2016`,
`2019`, `win10`, `win11`, `mixed`, `school`, `pki`); their files are in
[labs/](https://github.com/mxrguspxrt/win-test-lab/tree/main/labs). Run examples:
[examples/configs/](https://github.com/mxrguspxrt/win-test-lab/tree/main/examples/configs).

## More

- [docs/USING.md](https://github.com/mxrguspxrt/win-test-lab/blob/main/docs/USING.md): using win-test-lab from your own project, CI.
- [docs/CONFIG.md](https://github.com/mxrguspxrt/win-test-lab/blob/main/docs/CONFIG.md): every field of the lab and run files, what a run produces.
- [docs/ISOS.md](https://github.com/mxrguspxrt/win-test-lab/blob/main/docs/ISOS.md): getting and checking the Windows ISOs; the Arm64 drivers.
- [SPEC.md](https://github.com/mxrguspxrt/win-test-lab/blob/main/SPEC.md): how it works inside (QEMU, guest scripts, the report channel, settings).
- [docs/AGENTS.md](https://github.com/mxrguspxrt/win-test-lab/blob/main/docs/AGENTS.md): orientation for contributors and coding agents.

Tests that need no VM: `tests/all.sh` (in a clone). License: MIT.
