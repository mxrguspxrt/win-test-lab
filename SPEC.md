# win-test-lab: how it works (SPEC)

The internal contract between the host program, the guest scripts and the files they share.
For using the lab see [README.md](README.md) and [docs/CONFIG.md](docs/CONFIG.md).

## Pieces

    Cargo.toml, src/       the Rust host program `wtl` (crate `win-test-lab`)
      src/main.rs          the `wtl` command line
      src/lab.rs           the lab commands (run/up/.../doctor/check-iso/home/init); running
                           the base-image builds in parallel
      src/home.rs          the home: where ISOs, images and 0 states live, the config files
      src/assets.rs        guest/, lib/oslist.conf, labs/*.json built into the program
                           (listed by embed.rs, the build script)
      src/build.rs         unattended Windows install -> images/<name>.qcow2 (`wtl build-image`)
      src/winiso.rs        Windows ISO reader: UDF / ISO9660, WIM image list, build + CPU
      src/sha256.rs        SHA-256 for `wtl check-iso --sha256`
      src/labdef.rs        the JSON lab / run formats, validation, JSON Schema
      src/labrun.rs        the runner: 0 state, boot, control discs, steps, results
      src/qemu.rs          WTL_* settings and the QEMU command line (x64 and arm64)
      src/qmp.rs           QEMU monitor client (status, powerdown, quit, screendump -> PNG)
      src/serialwait.rs    wait for WTL events in a serial log
      src/outputs.rs       decode program output sent over serial
      src/mkiso.rs         ISO9660 + Joliet writer (control and mount discs) and reader
      src/oslist.rs        the OS table lib/oslist.conf
      src/sys.rs           process liveness / stop, detached spawn, Ctrl-C
    schema/                generated JSON Schemas of the two formats (`wtl schema`)
    tests/fake_lab.rs      end-to-end suite: `wtl` against a fake QEMU + qemu-img
    labs/<name>.json       lab definitions; labs/state/<name>/ their 0 state, runs, logs
    examples/configs/      example run configs
    guest/lab.ps1          guest side of the lab (rename, DC promotion, join, run steps)
    guest/labsvc.ps1       SYSTEM worker on each machine (mounts, steps as system / as a user)
    guest/agent.ps1        logon agent: owns COM1, dot-sources lab.ps1 from the control disc
    guest/setup.ps1        first-logon setup of a base image (build phase)
    guest/autounattend*.xml.tmpl   unattended Windows Setup answer files (BIOS / UEFI)
    drivers/               virtio-win drivers for arm64 machines (git-ignored; docs/ISOS.md)
    lib/oslist.conf        the OS table: key, Windows build, edition, firmware, CPU
    images-list.conf       optional, in the home: which ISO file serves which OS key (else
                           found by the build and CPU inside each ISO)
    examples/              smoke-test (C) and smoke-rs (Rust, x64 + Arm64) test programs,
                           pki/ scripts for the pki lab, configs/ example run files
    tests/                 all.sh (every no-VM test), ps_lint.py (PowerShell 4 lint),
                           check-labs.sh

Host requirements: QEMU (`qemu-system-x86_64` for x64 machines, `qemu-system-aarch64` for
arm64 machines, `qemu-img`) and its UEFI firmware; on Linux a writable `/dev/kvm`. Nothing
else: no bash, no Python, no root, no Samba, no xorriso/genisoimage, no checkout. `wtl` reads
the ISOs and writes its discs itself.

### CPUs and acceleration

Every OS key has a CPU (`arch` in lib/oslist.conf): `x64` or `arm64`. A machine whose CPU is
the host's own runs on the hypervisor (KVM / WHPX / HVF); any other runs emulated in QEMU's
TCG (`-accel tcg,thread=multi`), which works but is several times slower (see the measured
table in README.md). `wtl` scales its fixed waits (logon, power-off) by 6 for an emulated
machine. `WTL_ACCEL` forces one accelerator for every machine.

| | x64 machine | arm64 machine |
|---|---|---|
| QEMU | `WTL_QEMU`, machine `pc` (BIOS) or `q35` (UEFI) | `WTL_QEMU_ARM64`, machine `virt`, always UEFI |
| disk | ich9-ahci | NVMe |
| CD-ROMs | IDE | USB mass storage (the arm64 UEFI firmware boots no AHCI CD-ROM) |
| NICs | `WTL_NIC` (e1000) | virtio-net (driver: virtio-win NetKVM) |
| guest -> host log | COM1 (16550 UART) | virtio-serial port `org.wintestlab.0` (virtio-win vioserial), appended to the same serial log file |
| display | std VGA | ramfb |

Windows on Arm ships no driver for any NIC or UART QEMU emulates (tested: e1000e, rtl8139,
igb, vmxnet3, virtio-net, usb-net; PL011, pci-serial, usb-serial). The build therefore puts
virtio-win's ARM64 NetKVM and vioserial drivers under `\$WinPEDriver$` on the build disc,
which Windows Setup installs on its own, and refuses to start an arm64 build without them.

## Settings (environment)

| variable | default | meaning |
|----------|---------|---------|
| `WTL_HOME` (`--home`) | see below | the home: where the data lives (`WTL_ROOT` is an old name for it) |
| `WTL_CONFIG` | `config.json` in the per-user config folder | the user config file |
| `WTL_ISO_DIR` | `<home>/iso-files` | installer ISOs |
| `WTL_IMAGES_DIR` | `<home>/images` | base images `<os>-dc.qcow2`, `<os>-client.qcow2` |
| `WTL_STATE_DIR` | `<home>/labs/state` | each lab's 0 state, run disks, logs and results |
| `WTL_RUNS_DIR` | `<home>/runs` | base-image build scratch (`build-<image>/`) |
| `WTL_DRIVERS_DIR` | `<home>/drivers` | virtio-win drivers for arm64 machines |
| `WTL_QEMU`, `WTL_QEMU_IMG` | `qemu-system-x86_64`, `qemu-img` | the QEMU programs |
| `WTL_QEMU_ARM64` | `qemu-system-aarch64` | the QEMU program for arm64 machines |
| `WTL_ACCEL` | kvm / whpx / hvf by host for the host's CPU, tcg for the other | `kvm`, `whpx`, `hvf` or `tcg` for every machine; `WTL_NO_KVM=1` = tcg |
| `WTL_CPU` | `host,hv_*` on KVM, `max` otherwise | the `-cpu` value |
| `WTL_CPUS` | 2 | vCPUs when a machine sets none |
| `WTL_NIC` | `e1000` | NIC model |
| `WTL_QMP` | `unix` (`tcp` on Windows) | QEMU monitor transport |
| `WTL_OVMF_CODE`, `WTL_OVMF_VARS` | the first found: `/usr/share/OVMF/OVMF_{CODE,VARS}_4M.fd`, ..., Homebrew's `edk2-x86_64-code.fd` / `edk2-i386-vars.fd` | x64 UEFI firmware |
| `WTL_AAVMF_CODE`, `WTL_AAVMF_VARS` | the first found: `/usr/share/AAVMF/AAVMF_{CODE,VARS}.fd`, ..., Homebrew's `edk2-aarch64-code.fd` / `edk2-arm-vars.fd` | arm64 UEFI firmware |
| `WTL_BUILDER` | `wtl build-image` | what installs one base image (the tests use a fake) |
| `WTL_DISPLAY` | `gtk` (`cocoa` on macOS) | display used by `--show` |
| `WTL_PASSWORD` | `WinTest!2026` | local Administrator password baked into base images |
| `WTL_TEST_FAST=1` | | skip fixed settle sleeps (fake-QEMU tests only) |

The home (`src/home.rs`) is, first match wins: `WTL_HOME`; the current folder or a parent of
it with a `wtl.json` or a checkout (`lib/oslist.conf` + `guest/lab.ps1`); the checkout the
program was built in; `home` in the user config file; the per-user data folder
(`directories` crate: XDG on Linux, Application Support on macOS, `%LOCALAPPDATA%` on
Windows). Each folder above is its variable, else its key in the home's `wtl.json`, else its
key in the user config file, else its default; config paths are relative to their file. A
home that is a checkout supplies `guest/`, `lib/oslist.conf` and its `labs/`; otherwise the
copies built into the program are used, and a built-in lab NAME is written to
`<state_dir>/NAME/NAME.json` when used. A child `wtl build-image` gets the resolved folders in
its environment.

## Base images

One unattended Windows install per OS and role: `images/<os>-dc.qcow2` for domain
controllers and `images/<os>-client.qcow2` for every other machine (members, member servers,
workgroup machines). A clone of a DC cannot join its own domain, so the two are separate
installs; machines of the same OS and role share one image, in any lab. Any Server OS key
works as a DC and any OS key as the rest (a 2012 DC + 2019 member = `2012-dc` + `2019-client`).
`wtl run` runs itself as `wtl build-image <os> --name <os>-dc|<os>-client --computer-name
DC<os>|CL<os> --no-updates` for every missing image of a lab, in parallel, logging to
`labs/state/<lab>/build-<image>.log`.

The ISO: the home's `images-list.conf` entry for the OS key (`2019 = my-server-2019.iso`, a
path relative to the ISO folder) if its image 1 has the OS's build and CPU, else the newest ISO
in the ISO folder that does (read from the WIM inside; x64 and arm64
Windows 11 share build 26300, so the CPU matters). The build renders
`guest/autounattend*.xml.tmpl` (`@ARCH@` = amd64 / arm64), puts it with `guest/*.ps1`, a
`WTL/build.json` and, for arm64, the virtio-win drivers under `$WinPEDriver$` on a `WTL_BUILD`
disc, boots the installer ISO + that disc (UEFI: presses Enter for the "boot from CD" prompt
once the firmware starts a boot option), waits for `WTL BUILD_DONE` on the serial log and for
the guest to power off, then renames `<image>.qcow2.partial` to `<image>.qcow2`, keeps the
UEFI variable store as `<image>.vars.fd` (the Windows Boot Manager entry lives there; every
machine made from the image starts from a copy) and writes `<image>.json`
(`install_minutes` included). In the guest,
`setup.ps1` runs at first logon: autologon, firewall off, power settings, optional Windows
Update rounds, then installs the agent autostart (HKLM Run `WTL-Agent` -> `C:\WTL\agent.ps1`)
**after** sending `BUILD_DONE` (an agent started earlier would hold COM1), and shuts down.

## Lab modes and names

A lab with one `role: "dc"` machine is an **AD lab**: that machine becomes the DC of a new
forest, the others join it, `users` are domain users. A lab without one is a **workgroup**:
no domain, `users` are local accounts on every machine. One workgroup machine needs no lab
LAN (it gets only QEMU user-mode NAT); every other lab has the multicast LAN below.

The run file's `lab` is a name (`labs/<name>.json` in the home, else the built-in lab) or a
path relative to the run file. The lab's **state name** (`<state_dir>/<name>/`, VM names
`lab-<name>-<machine>`) is the lab's `name` field, else the lab name for a named lab, else the
file name plus an
FNV-1a hash of the lab file's absolute path (`lab-3f9a1c2b`), so labs from different
projects never collide.

## The 0 state

`labs/state/<lab>/<machine>-0.qcow2`: a qcow2 overlay per machine on its base image. It is
built once (`wtl prepare`, or the first `wtl run`) by booting the overlays themselves:

1. Each machine gets a control disc (`<machine>.iso`, label `WTL_LAB`, drive D:) with
   `LAB\lab.ps1`, `LAB\labsvc.ps1` and `LAB\lab.json` (below). The agent finds it at logon
   and dot-sources `lab.ps1`.
2. AD, the DC: set the local Administrator password (`admin_password`; it becomes the domain
   Administrator's); rename to the machine name + reboot; static IP; AD DS + new forest +
   reboot; create the groups and users; install its own roles/features/setup inline (it is
   already the domain Administrator) — e.g. `dhcp`; `LAB_DC_READY`; register the SYSTEM worker.
3. AD, the members (in parallel): static IP with the DC as DNS; register the SYSTEM worker;
   set the local Administrator password; wait for the DC; `Add-Computer -NewName <machine>`
   (join + rename, one reboot, retried for 20 min while the new DC finishes its first boot;
   an attempt that joined but failed the rename, e.g. "the directory service is busy", is
   detected from `Win32_ComputerSystem` and the rename is redone with `Rename-Computer`);
   autologon; `LAB_JOINED`; reboot. A member with roles/features/setup autologs on as the
   domain Administrator first (phase `configure`): install roles (`ad-cs`, `iis`, ...),
   features, then the setup scripts; switch autologon to its `logon` user; reboot. Then logged
   on as that user -> `LAB_CLIENT_READY`; `LAB_IDLE`.
3. Workgroup machines (in parallel): static IP on the lab LAN if there is one; register the
   SYSTEM worker; set the local Administrator password; create the lab's users as local
   accounts in their local groups (missing groups are created); rename to the machine name;
   a machine with roles/features/setup goes through the same `configure` phase as Administrator
   first; autologon as the machine's `logon` user (else Administrator); `LAB_JOINED`; reboot;
   logged on -> `LAB_CLIENT_READY`; `LAB_IDLE`.
4. The host powers every machine off with ACPI (members first, then the DC; a second press
   after 60 s if a user session delays it) and writes `0-state.json`.

The guest keeps its progress in `C:\WTL\lab-state.json` (`phase`: new, promoted, ready,
joined, configure), so every reboot continues where it stopped. Roles, features and setup
scripts (below) are applied here and frozen into the 0 state. `wtl rebuild` deletes the
`-0.qcow2` files and `0-state.json`; the base images stay.

## A run

`wtl run CONFIG`: restore (QMP quit, delete `<machine>.qcow2`), create new throwaway
overlays `<machine>.qcow2` on the 0-state disks, write each machine's control disc with
`run` filled in, write one mount disc per drive of the run for every machine that has steps
(`mount-<machine>-<letter>.iso`, label `WTL<LETTER>`, read-only), boot all machines (DC
first), wait for `LAB_DC_READY` and every member's `LAB_CLIENT_READY`, then read the serial
logs of all machines that have steps, round-robin, until each sent `LAB_RUN_DONE`. A
machine that powers itself off while being waited for is reset; a member that does not log
on within 5 min is reset (up to 3 times; domain autologon sometimes stops at the sign-in
screen).

Each step is checked against its `expect_exit` (or `exit=running` with `keep_running`).
Results go to `runs/<id>/` (see docs/CONFIG.md): `result.json`, `config.json`, and per
machine the decoded `stepNout/err.txt`, `serial.log`, `screen.png`. The VMs keep running;
`wtl up` is the same without steps.

Exit codes: 0 all steps passed, 1 a step failed, 2 lab error (bad config, `LAB_FAIL`, a VM
exited, an `ERROR` event; a screenshot `<machine>-fail.png` is taken), 3 timeout.

## The control disc's lab.json

Pure ASCII (PowerShell 4 reads it without a BOM), one per machine:

```json
{
 "fmt": "multi", "mode": "domain|workgroup", "role": "dc|member", "machine": "CL1",
 "lan": true, "ip": "10.77.30.20", "prefix": 24, "dc_ip": "10.77.30.10", "lan_mac": "52:54:00:4c:41:11",
 "domain": "lab.local", "netbios": "LAB", "admin_password": "...",
 "users": [{"name": "student1", "password": "...", "groups": ["Students"]}],
 "logon_user": "student1", "logon_password": "...",
 "shutdown_when_ready": false,
 "roles": ["ad-cs", "iis"], "features": ["Telnet-Client"],
 "ad_cs": {"ca_name": "LAB-ROOT-CA", "key_length": 2048, "validity_years": 5, "autoenroll": true},
 "dhcp": {"start": "10.77.30.100", "end": "10.77.30.199", "mask": "255.255.255.0", "router": "10.77.30.10"},
 "iis": {"site_path": "C:\\inetpub\\wwwroot"},
 "setup": [{"file": "setup/x.ps1", "args": "", "as_": "system|user", "user": "", "password": ""}],
 "run": null | {
   "mounts": [{"host": "/abs/host/path", "letter": "X"}],
   "steps": [{"n": 1, "as_": "system|logon|user", "user": "", "password": "",
              "path": "X:\\setup.bat", "args": "", "keep_running": false, "wait": 120}],
   "wait": 120 }
}
```

In workgroup mode `domain`, `netbios` and `dc_ip` are empty; `lan` is false (and `ip`
empty) for a machine without a lab LAN. `run` is set only for `wtl run`, and only on
machines that have steps. `as_` is resolved per machine: a step `as` the machine's own logon
user, or with no `as`, is `logon` (its desktop session); another lab user is `user` with that
user's password (started by the SYSTEM worker with `LogonUser` + `CreateProcessAsUser`, as
`NETBIOS\user` in AD mode and `.\user` in a workgroup; no desktop); `system` is the SYSTEM
worker. lab.ps1 refuses a disc whose `fmt` is not `multi`.

`roles`, `features`, `ad_cs`/`dhcp`/`iis` and `setup` are present only on the **build** disc
(they are baked into the 0 state; the guest acts on them in the `configure` phase) and absent
on a run disc. `dhcp` carries full addresses computed from the subnet; `router` is the DC.
Setup scripts are staged on the control disc under `LAB\<their relative path>` and run by the
SYSTEM worker.

`ad-cs` on a member (no ActiveDirectory module there) works through ADSI and the GroupPolicy
module (GPMC is installed if missing): `Install-AdcsCertificationAuthority -CAType
EnterpriseRootCA`; with `autoenroll`, create the template `WTLUser` (a schema-version-2 copy of
`User` with its own `msPKI-Enterprise-Oid`, auto-enroll flags, subject = directory path, UPN
in the SAN), grant Read/Enroll/AutoEnroll to Domain Users on it and to Domain Computers on
`Workstation`, add both names to the CA's `certificateTemplates` attribute
(`CN=<ca>,CN=Enrollment Services,...`; read back and retried, with the error text in the
`LAB_FAIL`), restart certsvc, and link the GPO with `AEPolicy = 7` (HKLM and HKCU) to the
domain root. At run start (AD labs) members run `gpupdate /force` + `certutil -pulse` before
their steps so auto-enrolled certificates are present.

## Privileges in the guest

The logged-on user runs `agent.ps1` (which owns COM1) and `lab.ps1`. A scheduled task
`WTL-Svc` (ONSTART, SYSTEM, registered while still Administrator) runs `C:\WTL\labsvc.ps1`,
which does what that user cannot: assign mount discs their drive letters, run `system`
steps and steps as another user. They talk through files: `C:\WTL\svc\<id>.req.json` ->
`C:\WTL\svc\<id>.res.json` (`{"ok":true,"exit":N,"secs":S}`, `{"ok":true,"timed_out":true}`,
`{"ok":true,"running":true}` or `{"ok":false,"error":"..."}`). Step output is captured to
`C:\WTL\run\stepN.out/err` and sent over serial by the agent's session.

## The network

With a `network`, every machine of a lab has a second NIC on one private LAN: QEMU `-netdev
dgram` on the UDP multicast group 239.77.30.1 (administratively scoped, never routed off the
host), port `network.lan_port`, local address 127.0.0.1. Any number of VMs join it without
root; labs are kept apart by their ports. MAC `52:54:00:4c:41:<10+i>`, static IPs from the
lab file. In an AD lab the DC is the DNS server of every NIC and has no other NIC (no
internet); every other machine also has QEMU user-mode NAT for internet access. A single
workgroup machine without `network` has only the NAT NIC.

## Serial protocol (guest -> host)

COM1 (on arm64: the virtio-serial port `\\.\Global\org.wintestlab.0`, which QEMU appends to
the same file), a file on the host (`labs/state/<lab>/<machine>.serial.log`), text lines.
Only administrators and SYSTEM may open the virtio-serial port, so on arm64 the logged-on lab
user's agent writes each line as a file into `C:\WTL\chan` and the SYSTEM worker
(`labsvc.ps1`) forwards them in order; the base-image build (Administrator) writes the port
directly. Lines look like:

    WTL <EVENT> [key=value ...] [| free text]

Values with characters outside `A-Za-z0-9._~:\/+,@()-` are URL-escaped. Lines without
`WTL ` are ignored (BIOS noise, reboots). Only the agent writes COM1.

| event | sent by | meaning |
|-------|---------|---------|
| `AGENT_START os=.. build=..` | agent.ps1 | someone logged on |
| `LAB_START role= phase= user=` | lab.ps1 | lab.ps1 started, with its saved phase |
| `LAB_PHASE \| text` | lab.ps1 | progress |
| `LAB_REBOOT phase=..` | lab.ps1 | about to reboot |
| `LAB_DC_READY domain= netbios= dc= ip=` | DC | domain up, users created |
| `LAB_JOINED domain=\|workgroup= autologon=` | member | joined (AD) / users made (workgroup), rebooting into the autologon |
| `LAB_CONFIGURE machine= role=` | lab.ps1 | 0-state build: starting the roles/features/setup of this machine |
| `LAB_ROLE machine= name= state=installing\|ready [..]` | lab.ps1 | a server role (`ad-cs`, `dhcp`, `iis`) is being / was installed |
| `LAB_FEATURE machine= name=` | lab.ps1 | a Windows feature was installed |
| `LAB_SETUP machine= n= as= script= state=running\|ok` | lab.ps1 | a setup script is running / succeeded |
| `LAB_CONFIGURED machine= autologon=` | member | 0-state build: configure done, rebooting into the logon user |
| `LAB_POLICY machine= state=refreshed` | lab.ps1 | run start (AD): gpupdate + certutil -pulse done |
| `LAB_CLIENT_READY user= domain=\|workgroup= computer= [dc=] ip=` | member | logged on as its logon user |
| `LAB_IDLE` | member | 0-state build: ready to be powered off |
| `LAB_MOUNT letter= host=` | lab.ps1 | a mount disc has its drive letter |
| `OUTPUT stream=stepNout\|stepNerr seq=N \| base64` | lab.ps1 | step output, 768-byte chunks (max 256 KiB per stream) |
| `OUTPUT_END stream= bytes= sent= truncated=0\|1` | lab.ps1 | end of a stream |
| `LAB_STEP machine= as= n= script= exit=N\|running\|timeout\|launchfail secs=` | lab.ps1 | a step finished |
| `LAB_RUN_DONE machine= steps=` | lab.ps1 | all of this machine's steps finished |
| `LAB_FAIL \| message (line N)` | lab.ps1 | the lab script failed; ends the run with exit 2 |

Build phase: `LOG`, `WU_ROUND`, `WU_GAVEUP`, `BUILD_DONE`, `BUILD_FAIL`.

## Guest PowerShell

All guest scripts must run on PowerShell 4.0 / .NET 4.5 (Server 2012 R2): no
`Expand-Archive`, no `::new()`, no classes, no `-AsHashtable`. `tests/ps_lint.py` checks a
subset. `lab.ps1` and `labsvc.ps1` are read from the control disc at every boot, but
`labsvc.ps1` is copied to `C:\WTL` (and the `WTL-Svc` task registered) during the 0-state
build: changes to it need `wtl rebuild`.
