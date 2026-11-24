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
- **Many environments from one lab.** `wtl up school --new` spawns a fresh copy of a whole lab
  in seconds, on its own private network; every machine in it knows which copy it is.
- **One run file for every case.** Variables (`WTL_OS=2022 wtl run smoke.json`) choose the
  Windows, the folders and the arguments; `extends` shares one file between many; secrets stay
  out of the results.
- **On Linux, macOS and Windows.** One program, `wtl`, plus QEMU. No bash, Python, root or
  Samba needed. On a Mac it currently supports Windows 11 on Arm only (see below).

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

## Variables, shared files and secrets

A run file can take parameters, so one file serves every Windows version and every build:

```json
{
  "vars": {
    "WTL_OS": "win11",
    "WTL_MODE": "quick",
    "WTL_BUILD": null,
    "WTL_LICENSE": { "secret": true }
  },
  "lab": "single-${WTL_OS}",
  "name": "smoke-${WTL_OS}",
  "mount": { "X": "${WTL_BUILD}" },
  "steps": [
    { "as": "system", "run": "X:/setup.ps1", "args": "-Mode ${WTL_MODE} -License ${WTL_LICENSE}" },
    { "run": "X:/smoke.exe" }
  ]
}
```

```
WTL_OS=2022 WTL_BUILD=./out WTL_LICENSE=ABCD-1234 wtl run smoke.json
for os in 2016 2019 2022 2025 win10 win11; do WTL_OS=$os WTL_BUILD=./out wtl run smoke.json; done
```

- **`vars`** are the file's parameters: each has a default, `null` for one you must set, or
  `{ "secret": true }`. An environment variable of the same name wins over the default.
- **`${WTL_NAME}`** works in any value: the lab, folders, programs, arguments. Only declared
  names are replaced (a typo is an error), and only `WTL_` names exist, so a run file can never
  read anything else from your environment (`GITHUB_TOKEN`, cloud keys...).
- **In Windows, too:** every declared variable is also set in each step's environment, so a
  script can read `$env:WTL_MODE` (PowerShell) or `%WTL_MODE%` (batch), whether it runs as
  SYSTEM, the logged-on user or another lab user.
- **Secrets** are shown as `***` in `result.json`, in `wtl`'s output and in its error messages;
  Windows still gets the real value.
- **`extends`** builds one file on another and changes only what differs; paths in the base
  stay relative to the base:

```json
{ "extends": "./smoke.json", "vars": { "WTL_OS": "2003" }, "mount": { "X": "./out32" } }
```

The whole set of rules:
[docs/CONFIG.md](https://github.com/mxrguspxrt/win-test-lab/blob/main/docs/CONFIG.md#variables-and-extends).

## Environments: many copies of one lab, on the fly

A lab's 0 state is built once. From it you can spawn as many **environments** as you like, with
no config files: each is a full copy of the lab (every machine, the domain, the CA), made in
seconds, running on its own private network.

```
wtl up school --new
  environment school-k7q3 (id 4b91d07c) made from lab school's 0 state
  ...
  environment school-k7q3 (id 4b91d07c) of lab school: domain lab.local
    DC     DC1   10.77.30.10  VNC 127.0.0.1:5910  LAB\Administrator
    member CL1   10.77.30.20  VNC 127.0.0.1:5911  logon LAB\student1
    member CL2   10.77.30.21  VNC 127.0.0.1:5912  logon LAB\student2

wtl up school --new --name exp42 --note "new installer on the 2016 client"
  environment school-exp42 (id c2e5a813) made from lab school's 0 state
```

Inside, an environment is exactly the lab: the same computer names (`DC1`, `CL1`), the same
domain, IPs and CA. Nothing in Windows is renamed. What tells the copies apart is their name and
id, which `wtl` and every machine in them know.

```
wtl list
  NAME           FROM     ID        STATE     NOTE
  school         -        8f3c1a2e  off
  school-k7q3    school   4b91d07c  running
  school-exp42   school   c2e5a813  running   new installer on the 2016 client
```

Every command takes an environment's name, and `NAME/MACHINE` is one machine in it:

```
wtl run smoke.json --lab school-exp42    # run a file's steps in that environment
wtl run smoke.json --new                 # ... or in a fresh one, made for this run
wtl status school-exp42                  # its machines, VNC addresses, last run
wtl status school-exp42/CL1              # one machine
wtl restore school-exp42                 # back to the 0 state it was made from
wtl stop school-exp42                    # power off, keep its changes
wtl rm school-exp42                      # delete it: VMs, disks, results
```

- **Fast and small.** An environment is a thin disk layer on the lab's 0 state: no reinstall,
  no rebuild, and it only takes the disk space its own changes need.
- **Isolated.** Each environment gets its own private LAN, so `school`, `school-k7q3` and
  `school-exp42` run side by side and never see each other, although their machines have the
  same names and IPs.
- **Named for you.** Without `--name` the name is the lab plus 4 random characters. The `id` is
  always random, so two people's `school-exp42` never look the same.
- **Rebuilding is safe.** `wtl rebuild school` deletes the lab's 0 state (the next run or
  `--new` builds a new one); existing environments keep the 0 state they were made from (kept
  on disk until the last one using it is removed). `wtl rebuild school-exp42` makes that
  environment fresh again from the lab's current 0 state, with the same name, id and note.

### Where am I? Inside Windows

Every machine of every environment can tell which lab, environment, machine and run it is, so
your program or test knows where it is running. At every boot `wtl` writes it to
`C:\WTL\instance.json`:

```json
{
  "lab": "school",
  "environment": "school-exp42",
  "id": "c2e5a813",
  "machine": "CL1",
  "note": "new installer on the 2016 client",
  "run_id": "20261124-101500-c2e5a813"
}
```

and the same values to the registry (`HKLM\SOFTWARE\WinTestLab`) and to every step's environment
(`WTL_ENVIRONMENT`, `WTL_ENVIRONMENT_ID`, `WTL_MACHINE`, `WTL_RUN_ID`):

```powershell
$me = Get-Content C:\WTL\instance.json | ConvertFrom-Json
"I am $($me.machine) in $($me.environment) ($($me.id))"
```

```bat
reg query HKLM\SOFTWARE\WinTestLab /v Environment
```

`run_id` is empty outside `wtl run` (after `wtl up`). On the host, `result.json` carries the same
`environment`, `id` and `run_id`.

[examples/envs](https://github.com/mxrguspxrt/win-test-lab/tree/main/examples/envs) walks
through all of it, one command at a time, with `whereami.exe`: a small program that checks
inside each machine that the computer name, `instance.json`, the registry and the step
variables all say the same, and `run-all.sh`, which runs the whole walkthrough with every
result checked (on one machine, Server 2003, or an AD lab running next to its own copy).

## Which host runs what

`wtl` runs on Linux, macOS and Windows. Windows runs fast only where its CPU matches the
host's, so which Windows you can test depends on the host:

| host | supported Windows | how |
|------|-------------------|-----|
| **Linux** (x64) | every one: Server 2003 R2, 2008 R2, 2012 R2 to 2025, Windows 10, Windows 11 | native, KVM. Tested. |
| **macOS** (Apple Silicon) | **Windows 11 on Arm only** | native, Apple's hypervisor. Tested. |
| **Windows** (x64) | every one except Server 2003 R2 and 2008 R2 (they need libguestfs, Linux only) | native, Windows Hypervisor Platform. Compiles, not tested yet. |

**The macOS limit:** an Apple Silicon Mac has an Arm CPU, and Microsoft publishes an Arm64 ISO
only for Windows 11; there is no Windows Server for Arm. Any x64 Windows (every Server, Windows
10, x64 Windows 11) can only be emulated there, and that is too slow to use: a Server 2025
install took 73 minutes and PowerShell needed ~50 s just to start. So on a Mac, `wtl` supports
Windows 11 on Arm (which still runs your x64 programs, see below); for Servers and AD domains
use a Linux or Windows machine. An Intel Mac runs x64 Windows natively, but is not tested.

## Install

You need QEMU and Rust (https://rustup.rs). On Ubuntu/Debian,
[`install-deps.sh`](https://github.com/mxrguspxrt/win-test-lab/blob/main/install-deps.sh) installs the lot and adds you to the `kvm` group:

```
./install-deps.sh          # core deps;  --all also adds the optional ones below
```

Or install them yourself:

| host | QEMU and firmware |
|------|------|
| Debian / Ubuntu | `sudo apt install qemu-system-x86 qemu-system-arm qemu-utils ovmf qemu-efi-aarch64` (and `sudo usermod -aG kvm $USER`) |
| Fedora | `sudo dnf install qemu-kvm qemu-img edk2-ovmf` |
| macOS | `brew install qemu` |
| Windows | `winget install SoftwareFreedomConservancy.QEMU`, and turn on the Windows feature "Windows Hypervisor Platform" |

What each piece is for:

- **`qemu-system-x86` + `qemu-utils` + `ovmf`** — the minimum: run any x64 Windows (every Server,
  10, 11 x64) and build its images. `ovmf` is the x64 UEFI firmware (Windows 11 needs it).
- **`qemu-system-arm` + `qemu-efi-aarch64`** — only to run Windows 11 on Arm.
- **`libguestfs-tools`** (Linux only) — only for **Windows Server 2003 R2 and 2008 R2**, which are
  imported from a VHD rather than installed from an ISO (see
  [docs/ISOS.md](https://github.com/mxrguspxrt/win-test-lab/blob/main/docs/ISOS.md)). `wtl`
  uses `virt-customize` to drop an unattended-setup answer file into the imported image. On
  Debian/Ubuntu the kernel must also be readable (`sudo chmod 0644 /boot/vmlinuz-*`) or
  libguestfs's supermin fails — `install-deps.sh` does this.
- Optional: **`gcc-mingw-w64-x86-64`** (and **`gcc-mingw-w64-i686`**, for the 32-bit Server 2003)
  to build the bundled example program, **`unar`** to unpack the 2003/2008 VHD downloads.

`wtl doctor` reports what your host can actually run.

```
cargo install win-test-lab    # installs the `wtl` command (`cargo install wtl` is the same)
wtl doctor                    # what this computer can run, which ISOs you have
```

Or from a clone: `cargo build --release`, then use `target/release/wtl`.

Windows Server 2003 and 2008 R2 are a special case: Microsoft never published a free install
ISO for them, only pre-installed **VHD Test Drive images**, so `wtl` imports the VHD instead of
installing from an ISO. That needs one extra tool, libguestfs, to inject the first-boot hook
(`sudo apt install libguestfs-tools`, Fedora `sudo dnf install guestfs-tools`); every other
Windows needs only QEMU. See [docs/ISOS.md](https://github.com/mxrguspxrt/win-test-lab/blob/main/docs/ISOS.md).

## Download the Windows installers

`wtl` installs Windows from Microsoft's own files. They are not part of this project (they are
big, and Microsoft's terms do not allow redistributing them), so you download them yourself:
click the link for each Windows you want, accept Microsoft's terms in your browser, download,
and put the file in the **ISO folder** (`wtl home` shows where; `iso-files/` by default). Use
Microsoft's pages only, no third-party download tools or mirrors.

| Lab key | Windows | Download | What to get |
|---------|---------|----------|-------------|
| `2003` | Windows Server 2003 R2 Enterprise, 32-bit | [Test Drive VHD](https://www.microsoft.com/en-us/download/details.aspx?id=19727) | all 4 parts (`Win2k3R2EE.part01.exe` to `part04.rar`) |
| `2008` | Windows Server 2008 R2 Enterprise, x64 | [Test Drive VHD](https://www.microsoft.com/en-us/download/details.aspx?id=2227) | all 3 parts (`WS2008R2Fullx64Ent.part01.exe` to `part03.rar`) |
| `2012` | Windows Server 2012 R2 | [Evaluation Center](https://www.microsoft.com/en-us/evalcenter/evaluate-windows-server-2012-r2) | the ISO |
| `2016` | Windows Server 2016 | [Evaluation Center](https://www.microsoft.com/en-us/evalcenter/evaluate-windows-server-2016) | the ISO |
| `2019` | Windows Server 2019 | [Evaluation Center](https://www.microsoft.com/en-us/evalcenter/evaluate-windows-server-2019) | the ISO |
| `2022` | Windows Server 2022 | [Evaluation Center](https://www.microsoft.com/en-us/evalcenter/evaluate-windows-server-2022) | the ISO |
| `2025` | Windows Server 2025 | [Evaluation Center](https://www.microsoft.com/en-us/evalcenter/download-windows-server-2025) | the ISO |
| `win10` | Windows 10 Pro 22H2 | [Download Windows 10](https://www.microsoft.com/en-us/software-download/windows10ISO) | the 64-bit ISO |
| `win11` | Windows 11 Pro | [Download Windows 11](https://www.microsoft.com/en-us/software-download/windows11) | the "Disk Image (ISO)" for x64 |
| `win11-arm64` | Windows 11 Pro on Arm | [Download Windows 11 on Arm](https://www.microsoft.com/en-us/software-download/windows11arm64) | the Arm64 ISO |

- **ISOs (2012 R2 and newer):** the file name does not matter, `wtl` reads the Windows build and
  CPU inside each ISO. For the Servers pick the **ISO** (not Azure or VHD), your language and the
  **Standard** or **Datacenter** edition with *Desktop Experience*; the evaluations need no
  product key and run for 180 days. Windows 10 and 11 install as Pro, not activated.
- **VHDs (2003 R2 and 2008 R2):** each is a split self-extracting archive. Put all its parts in
  one folder under the ISO folder and unpack the first one (`unar Win2k3R2EE.part01.exe`, or run
  the `.exe` on Windows) so a `.vhd` appears. Leave it anywhere under the ISO folder: `wtl`
  recognises it by what is inside, whatever the file is called. They are Microsoft evaluation
  copies, so read the licence that comes with each, and they need libguestfs (see above).
- `wtl doctor` lists which of them it found; for a missing VHD it prints the link. Details,
  including the CPU of each OS:
  [docs/ISOS.md](https://github.com/mxrguspxrt/win-test-lab/blob/main/docs/ISOS.md).

## Start

Once the installers you need are in the ISO folder:

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
user, exit code, duration; the variables used), each step's output, and a screenshot of each
machine. Several labs can run at the same time: each VM finds its own free VNC port, and labs
with a network need different `lan_port`s.

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

## On a Mac: Windows 11 on Arm

Windows 11 on Arm runs natively on an Apple Silicon Mac (Apple's hypervisor, no emulation),
and it runs **both Arm64 and x64 programs**: Windows translates an x64 exe on the fly, per
program, with nothing to set up. So you can test your x64 build on a Mac too, as long as it
needs Windows 11 and not a Server.

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

The other way round, a Linux or Windows x64 host can run Windows 11 on Arm only emulated
(slow), so Arm64-only testing belongs on a Mac.

## Commands

    wtl run CONFIG          restore, boot, mount, run the steps, report (exit 0 = all steps passed)
                            (--lab ENV: in that environment; --new: in a fresh one)
    wtl up LAB              boot the lab from its 0 state and leave it running
    wtl up LAB --new        spawn a new environment of the lab (--name N, --note TEXT)
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

Exit codes: 0 ok, 1 a step failed, 2 lab error, 3 timeout. `--show` opens the VM windows
instead of VNC. Wherever a command takes a LAB, it also takes an environment's name.

Labs built into `wtl`: one machine each (`single-2003`, `single-2008`, `single-2012`,
`single-2016`, `single-2019`, `single-2022`, `single-2025`, `single-win10`, `single-win11`,
`single-win11-arm64`), AD labs (`2012`, `2016`,
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
