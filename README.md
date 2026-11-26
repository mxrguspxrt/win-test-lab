# win-test-lab

**Test your software on real Windows, from Server 2003 to Windows 11, with one JSON file.**

`wtl` installs Windows for you, unattended, from Microsoft's own installers; keeps each setup as a
clean snapshot (the "0 state"); boots it in QEMU; runs your program inside; and gives you back
every step's exit code and output. No clicking through installers, no forgotten setup steps, and
a fresh Windows on every run.

- **Every Windows:** Server 2003 R2, 2008 R2, 2012 R2, 2016, 2019, 2022 and 2025, Windows 10 and
  Windows 11 (x64 and Arm64).
- **Clean every time:** a lab is built once; every run starts from it, and `wtl restore` resets
  it in about a second.
- **Real networks:** one machine, or an Active Directory domain with DHCP, a certificate
  authority (AD CS) and IIS.
- **Any user:** each step runs as SYSTEM, as the logged-on user or as any other lab user.
- **Many copies at once:** `wtl up LAB --new` makes a full copy of a lab in seconds, on its own
  network, and can give its machines new computer names.
- **VMs inside Windows:** a lab machine can run virtual machines of its own, on the hardware
  (nested virtualization).
- **Linux, macOS and Windows hosts:** one program plus QEMU. No root, Samba, bash or Python.
- **Made for coding agents:** Claude Code can build your program, `wtl run` it on every Windows
  you care about, read the results and fix what failed, on its own.

## Contents

- [Quick start](#quick-start)
- [Tested](#tested)
- [Lab files and run files](#lab-files-and-run-files)
- [Environments: copies of a lab](#environments-copies-of-a-lab)
- [Virtual machines inside Windows](#virtual-machines-inside-windows)
- [Which host runs what](#which-host-runs-what)
- [Install](#install)
- [Download the Windows installers](#download-the-windows-installers)
- [Where the data lives](#where-the-data-lives)
- [Commands](#commands)

## Quick start

1. [Install](#install) QEMU and `wtl`, then check what your computer can run:

   ```
   cargo install win-test-lab
   wtl doctor
   ```

2. [Download a Windows installer](#download-the-windows-installers) from Microsoft into the ISO
   folder (`wtl home` shows where it is).

3. Write a run file, `smoke.json`: one Windows 11 machine, your build folder as drive `X:`, two
   steps.

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

4. Run it:

   ```
   wtl run smoke.json
   ```

The first run installs Windows (10-50 minutes, once) and builds the lab's 0 state. After that a
run takes a few minutes. A real run of the bundled smoke test
([examples/configs/single-smoke.json](https://github.com/mxrguspxrt/win-test-lab/blob/main/examples/configs/single-smoke.json),
`WTL_OS=win11`), without the guest's own report lines:

```
[03:00:58] lab single-win11: booting 1 machines from the 0 state
[03:00:58]   WSWIN11 mount X: <- examples/smoke-test/out (256K)
[03:00:58]   WSWIN11 started (VNC 127.0.0.1:5916)
[03:02:59] WSWIN11 ready
[03:02:59] running single-smoke.json (steps on 1 machine(s))
[03:04:15] [WSWIN11 system] X:\setup.bat -> exit 0
[03:04:16] [WSWIN11 logon] X:\smoke.exe -> exit 0
[03:04:16] [WSWIN11] all steps done

  lab single-win11: workgroup
    single WSWIN11         (NAT only)  VNC 127.0.0.1:5916  logon WSWIN11\tester
  back to the 0 state: wtl restore single-win11      another run: wtl run CONFIG
  config: single-smoke.json  ->  ALL STEPS OK
  results: labs/state/single-win11/runs/20261126-030058 (per machine: <name>/step*.txt, screen.png; result.json = summary)
```

The results are in the lab's state folder, `runs/<time>/`: `result.json` (every step with its
machine, user, exit code and duration), each step's output, and a screenshot of each machine.
`wtl` exits with 0 when every step passed, 1 when a step failed, 2 on a lab error and 3 on a
timeout, so it fits in a script or CI.

## Tested

Every lab that comes with `wtl` passes on real VMs: on Linux with KVM (AMD Ryzen), and Windows
11 on Arm on an Apple Silicon Mac. Each one was installed from Microsoft's own installer, built
into its 0 state and then run from it: the bundled smoke test (a setup script as SYSTEM, then a
program as the logged-on user), and in `pki` also its certificates, DHCP and IIS.

| Windows | one machine | in an Active Directory domain | also tested |
|---------|-------------|-------------------------------|-------------|
| Server 2003 R2 (32-bit) | `single-2003` | (standalone only) | environments, `--rename` |
| Server 2008 R2 | `single-2008` | (standalone only) | environments, `--rename` |
| Server 2012 R2 | `single-2012` | `2012`; DC of `mixed`; CA + IIS host in `pki`; member in `school` | |
| Server 2016 | `single-2016` | `2016`; members in `pki` and `school` | |
| Server 2019 | `single-2019` | `2019`; DC of `pki`, `school`, `win10`, `win11`; member in `mixed` | environments side by side, `--rename` of DC and member |
| Server 2022 | `single-2022` | | environments |
| Server 2025 | `single-2025` | | |
| Windows 10 | `single-win10` | `win10` | |
| Windows 11 | `single-win11` | `win11` | virtual machines inside it, hardware-accelerated |
| Windows 11 on Arm | `single-win11-arm64` (Mac) | | x64 programs, translated by Windows |

## Lab files and run files

`wtl` reads two kinds of file: a **lab** file says which machines there are, and a **run** file
says what to run on them. An Active Directory lab: a 2019 DC with DHCP, a 2012 R2 server with an
Enterprise CA and IIS, and a 2016 member that a domain user logs on to:

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

A run on it, each step on the machine and as the user it names:

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

Both formats have JSON Schemas (`wtl schema`, or
[schema/](https://github.com/mxrguspxrt/win-test-lab/tree/main/schema)), so your editor completes
and checks them. Labs that come with `wtl`: one machine each (`single-2003`, `single-2008`,
`single-2012`, `single-2016`, `single-2019`, `single-2022`, `single-2025`, `single-win10`,
`single-win11`, `single-win11-arm64`) and AD labs (`2012`, `2016`, `2019`, `win10`, `win11`,
`mixed`, `school`, `pki`), in [labs/](https://github.com/mxrguspxrt/win-test-lab/tree/main/labs).

**Variables** make one run file serve every Windows and every build:

```json
{
  "vars": { "WTL_OS": "win11", "WTL_BUILD": null, "WTL_TOKEN": { "secret": true } },
  "lab": "single-${WTL_OS}",
  "mount": { "X": "${WTL_BUILD}" },
  "steps": [
    { "as": "system", "run": "X:/setup.ps1", "args": "-Token ${WTL_TOKEN}" },
    { "run": "X:/smoke.exe" }
  ]
}
```

```
for os in 2016 2019 2022 2025 win10 win11; do WTL_OS=$os WTL_BUILD=./out wtl run smoke.json; done
```

- A variable has a default, `null` (you must set it) or `{ "secret": true }`; an environment
  variable of the same name wins. Only declared `WTL_` names exist, so a run file can never read
  anything else from your environment.
- Steps get every variable in their environment too (`$env:WTL_OS`, `%WTL_OS%`).
- Secrets show as `***` in `result.json` and in `wtl`'s output; Windows gets the real value.
- `extends` builds one run file on another and changes only what differs.

Every field of both files:
[docs/CONFIG.md](https://github.com/mxrguspxrt/win-test-lab/blob/main/docs/CONFIG.md).

## Environments: copies of a lab

A lab's 0 state is built once. From it, `wtl up LAB --new` makes an **environment** in seconds: a
full copy of the lab (every machine, the domain, the CA) on its own private network, so copies
with the same machine names and IPs run side by side.

```
wtl up school --new --name exp42 --note "new installer"   # a copy: school-exp42
wtl list                                                  # labs and environments, running or off
wtl run smoke.json --lab school-exp42                     # a run in that copy
wtl run smoke.json --new --rm                             # ... or in a fresh copy, deleted afterwards
wtl up school --new --rename all                          # real new computer names: DC1-K7Q3, ...
wtl rm school-exp42                                       # delete it
```

An environment is a thin disk layer on the lab's 0 state, so it takes only the space of its own
changes. Every step knows where it runs: `WTL_ENVIRONMENT`, `WTL_MACHINE`, `WTL_HOSTNAME`,
`WTL_RUN_ID` and more are set in its environment. All of it, with `--rename` and its rules:
[docs/ENVIRONMENTS.md](https://github.com/mxrguspxrt/win-test-lab/blob/main/docs/ENVIRONMENTS.md).

## Virtual machines inside Windows

A lab machine can run virtual machines of its own. `wtl` gives Windows the host CPU's
virtualization (AMD-V or VT-x; on Linux, KVM's `nested` option is on by default), so with the
Windows Hypervisor Platform turned on, QEMU for Windows runs VMs on the hardware
(`-accel whpx`), and QEMU's software CPU (`-accel tcg`) works in any lab machine.
[examples/nested](https://github.com/mxrguspxrt/win-test-lab/tree/main/examples/nested) builds a
Windows 11 lab with the Hypervisor Platform and QEMU baked in, and boots a tiny VM inside it
on each:

```
examples/nested/build.sh                 # once: the tiny VM and QEMU for Windows
wtl run examples/nested/nested.json      # the first time it also builds the lab
```

Under whpx the tiny VM runs on Windows' Hyper-V on the host's KVM, three hypervisors deep, and
still at the hardware's speed: 6 to 14 times faster than on the software CPU, and it boots and
exits in 1-4 s. A whole Windows does not boot inside yet (Server 2003 stops at its boot screen).

## Which host runs what

Windows runs fast only where its CPU matches the host's, so the host decides which Windows you
can test:

| host | Windows | how |
|------|---------|-----|
| **Linux** (x64) | all of them | KVM. Tested. |
| **macOS** (Apple Silicon) | **Windows 11 on Arm only** | Apple's hypervisor. Tested. |
| **Windows** (x64) | all but Server 2003 R2 and 2008 R2 (they need libguestfs, Linux only) | Windows Hypervisor Platform. Compiles, not tested yet. |

An Apple Silicon Mac has an Arm CPU, and the only Windows with an Arm64 installer is Windows 11.
Any x64 Windows would be emulated there, much too slowly (a Server 2025 install took 73
minutes). Windows 11 on Arm still runs your **x64 programs**, translated by Windows, so you can
test an x64 build on a Mac as long as it needs Windows 11 and not a Server.
[examples/smoke-rs](https://github.com/mxrguspxrt/win-test-lab/tree/main/examples/smoke-rs) runs
one program built for Arm64 and for x64 there:

```
smoke.exe running as WSARM64\tester
exe built for arm64, machine CPU arm64
RESULT OK

smoke.exe running as WSARM64\tester
exe built for x64, machine CPU arm64 (running translated)
RESULT OK
```

On an M-series MacBook Air: installing Windows 11 on Arm takes 48 minutes (once), building a lab
2 minutes (once per lab), a run 3 minutes. It needs two drivers from virtio-win, see
[docs/ISOS.md](https://github.com/mxrguspxrt/win-test-lab/blob/main/docs/ISOS.md#drivers-for-arm64-machines-virtio-win).

## Install

`wtl` needs QEMU, and Rust (https://rustup.rs) to install it:

```
cargo install win-test-lab    # the `wtl` command (cargo install wtl is the same)
wtl doctor                    # what this computer can run, which installers you have
```

Or from a clone: `cargo build --release`, then use `target/release/wtl`. On Ubuntu or Debian,
[`install-deps.sh`](https://github.com/mxrguspxrt/win-test-lab/blob/main/install-deps.sh)
installs QEMU and the rest and adds you to the `kvm` group (`--all` adds the optional tools).
Or by hand:

| host | QEMU and firmware |
|------|-------------------|
| Debian / Ubuntu | `sudo apt install qemu-system-x86 qemu-utils ovmf` and `sudo usermod -aG kvm $USER` |
| Fedora | `sudo dnf install qemu-kvm qemu-img edk2-ovmf` |
| macOS | `brew install qemu` |
| Windows | `winget install SoftwareFreedomConservancy.QEMU`, and the Windows feature "Windows Hypervisor Platform" |

- Windows 11 on Arm also needs `qemu-system-arm qemu-efi-aarch64`.
- Server 2003 R2 and 2008 R2 also need **libguestfs** (`libguestfs-tools`; Fedora
  `guestfs-tools`), Linux only. On Debian and Ubuntu the kernel must be readable for it
  (`sudo chmod 0644 /boot/vmlinuz-*`; `install-deps.sh` does it).
- Optional: `gcc-mingw-w64-x86-64` (and `gcc-mingw-w64-i686` for the 32-bit Server 2003) to build
  the example programs, `unar` to unpack the 2003 and 2008 downloads.

## Download the Windows installers

`wtl` installs Windows from Microsoft's own files. They are not part of this project (they are
big, and Microsoft's terms do not allow passing them on), so you download them yourself from
Microsoft's pages below (no third-party mirrors) and put them in the **ISO folder** (`wtl home`
shows where; `iso-files/` by default). The file names do not matter: `wtl` recognises each file
by what is inside it.

| key | Windows | download | what to get |
|-----|---------|----------|-------------|
| `2003` | Server 2003 R2 Enterprise, 32-bit | [Test Drive VHD](https://www.microsoft.com/en-us/download/details.aspx?id=19727) | all 4 parts (`Win2k3R2EE.part01.exe` to `part04.rar`) |
| `2008` | Server 2008 R2 Enterprise, x64 | [Test Drive VHD](https://www.microsoft.com/en-us/download/details.aspx?id=2227) | all 3 parts (`WS2008R2Fullx64Ent.part01.exe` to `part03.rar`) |
| `2012` | Server 2012 R2 | [Evaluation Center](https://www.microsoft.com/en-us/evalcenter/evaluate-windows-server-2012-r2) | the ISO |
| `2016` | Server 2016 | [Evaluation Center](https://www.microsoft.com/en-us/evalcenter/evaluate-windows-server-2016) | the ISO |
| `2019` | Server 2019 | [Evaluation Center](https://www.microsoft.com/en-us/evalcenter/evaluate-windows-server-2019) | the ISO |
| `2022` | Server 2022 | [Evaluation Center](https://www.microsoft.com/en-us/evalcenter/evaluate-windows-server-2022) | the ISO |
| `2025` | Server 2025 | [Evaluation Center](https://www.microsoft.com/en-us/evalcenter/download-windows-server-2025) | the ISO |
| `win10` | Windows 10 Pro 22H2 | [Download Windows 10](https://www.microsoft.com/en-us/software-download/windows10ISO) | the 64-bit ISO |
| `win11` | Windows 11 Pro | [Download Windows 11](https://www.microsoft.com/en-us/software-download/windows11) | the "Disk Image (ISO)" for x64 |
| `win11-arm64` | Windows 11 Pro on Arm | [Download Windows 11 on Arm](https://www.microsoft.com/en-us/software-download/windows11arm64) | the Arm64 ISO |

- **Servers:** pick the **ISO** (not Azure or VHD), your language, and the Standard or
  Datacenter edition with *Desktop Experience*. The evaluations need no product key and run for
  180 days. Windows 10 and 11 install as Pro, not activated.
- **Server 2003 R2 and 2008 R2** have no install ISO, only Microsoft's pre-installed Test Drive
  VHDs, which `wtl` imports. Each is a split self-extracting archive: put its parts in one
  folder under the ISO folder and unpack the first (`unar Win2k3R2EE.part01.exe`, or run the
  `.exe` on Windows). They are evaluation copies: read the licence that comes with each.
- `wtl doctor` and `wtl check-iso` show what they found. More, with the CPU of each OS:
  [docs/ISOS.md](https://github.com/mxrguspxrt/win-test-lab/blob/main/docs/ISOS.md).

## Where the data lives

Installers, base images (20-30 GB each) and 0 states live in one folder, the **home**. `wtl home`
shows which folder it is and why. The first match wins:

1. `--home DIR`, or the `WTL_HOME` variable
2. the current folder or a parent with a `wtl.json` (`wtl init` writes one), or a win-test-lab
   checkout
3. `home` in the user config file (`~/.config/win-test-lab/config.json`; macOS
   `~/Library/Application Support/win-test-lab/config.json`; Windows
   `%APPDATA%\win-test-lab\config\config.json`)
4. the per-user data folder: `~/.local/share/win-test-lab`, macOS
   `~/Library/Application Support/win-test-lab`, Windows `%LOCALAPPDATA%\win-test-lab\data`

Each folder can also be moved on its own, e.g. the images to a bigger disk, with a variable or
a key in `wtl.json` or the user config file (paths there are relative to that file):

| folder | variable | config key | default |
|--------|----------|------------|---------|
| Windows installers | `WTL_ISO_DIR` | `iso_dir` | `iso-files/` |
| base images | `WTL_IMAGES_DIR` | `images_dir` | `images/` |
| 0 states, runs, logs | `WTL_STATE_DIR` | `state_dir` | `labs/state/` |
| image build scratch | `WTL_RUNS_DIR` | `runs_dir` | `runs/` |
| Arm64 drivers | `WTL_DRIVERS_DIR` | `drivers_dir` | `drivers/` |

## Commands

    wtl run CONFIG          restore, boot, mount, run the steps, report (exit 0 = all steps passed)
                            (--lab ENV: in that environment; --new: in a fresh one,
                            with --rename too, and --rm to delete it afterwards)
    wtl up LAB              boot the lab from its 0 state and leave it running
    wtl up LAB --new        make a new environment of the lab (--name N, --note TEXT,
                            --rename KEY=NAME|KEY|all: real new computer names)
    wtl list                every lab and environment: id, running or off, note
    wtl status LAB          0 state, machines (running/off, VNC), last run (LAB/MACHINE: one)
    wtl restore LAB         stop and throw away everything since the 0 state
    wtl stop LAB            power off, keeping changes until the next run
    wtl rm ENV              delete an environment: VMs, disks, results
    wtl prepare LAB         build the 0 state now
    wtl rebuild LAB         a new 0 state; the next run builds it (environments keep theirs)
    wtl doctor              what this host can run: QEMU per CPU, acceleration, firmware, ISOs
    wtl check-iso [FILE]    an ISO's Windows build, CPU and editions (--sha256 HASH to verify it)
    wtl home                where the data is, and where each setting comes from
    wtl init [DIR]          make DIR a home (writes DIR/wtl.json)
    wtl build-image OS      install one base image now (wtl run does it when one is missing)
    wtl schema [DIR]        write the JSON Schemas of the lab and run files

Wherever a command takes a LAB, it also takes an environment's name. `--show` opens the VM
windows instead of VNC. Several labs can run at once: each VM finds a free VNC port, and labs
with a network need different `lan_port`s.

## More

- [docs/USING.md](https://github.com/mxrguspxrt/win-test-lab/blob/main/docs/USING.md): using win-test-lab from your own project, and in CI.
- [docs/CONFIG.md](https://github.com/mxrguspxrt/win-test-lab/blob/main/docs/CONFIG.md): every field of the lab and run files, and what a run produces.
- [docs/ENVIRONMENTS.md](https://github.com/mxrguspxrt/win-test-lab/blob/main/docs/ENVIRONMENTS.md): environments, `--rename`, and what each step knows about where it runs.
- [docs/ISOS.md](https://github.com/mxrguspxrt/win-test-lab/blob/main/docs/ISOS.md): getting and checking the Windows installers; the Arm64 drivers.
- [SPEC.md](https://github.com/mxrguspxrt/win-test-lab/blob/main/SPEC.md): how it works inside (QEMU, guest scripts, the report channel, settings).
- [docs/AGENTS.md](https://github.com/mxrguspxrt/win-test-lab/blob/main/docs/AGENTS.md): orientation for contributors and coding agents.

Tests that need no VM: `tests/all.sh` (in a clone). License: MIT.
