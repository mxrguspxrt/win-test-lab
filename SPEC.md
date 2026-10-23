# win-test-lab: how it works (SPEC)

The internal contract between the host program, the guest scripts and the files they share.
For using the lab see [README.md](README.md) and [docs/CONFIG.md](docs/CONFIG.md).

## Pieces

    wtl-rs/                Rust host program `wtl-rs` (also runs as `lab`): the AD lab runner
      src/lab.rs           `lab` command line; finding the checkout; driving build-image.sh
      src/labdef.rs        the JSON lab / run formats, validation, JSON Schema
      src/labrun.rs        the runner: 0 state, boot, control discs, steps, results
      src/qemu.rs          WTL_* settings and the QEMU command line
      src/qmp.rs           QEMU monitor client (status, powerdown, quit, screendump -> PNG)
      src/serialwait.rs    wait for WTL events in a serial log
      src/outputs.rs       decode program output sent over serial
      src/mkiso.rs         ISO9660 + Joliet writer (control and mount discs) and reader
      src/oslist.rs        the OS table lib/oslist.conf
      src/sys.rs           process liveness / stop, detached spawn, Ctrl-C
      schema/              generated JSON Schemas of the two formats (`wtl-rs schema`)
      tests/fake_lab.rs    end-to-end suite: wtl-rs lab against a fake QEMU + qemu-img
    labs/<name>.json       lab definitions; labs/state/<name>/ their 0 state, runs, logs
    examples/configs/      example run configs
    guest/lab.ps1          guest side of the lab (rename, DC promotion, join, run steps)
    guest/labsvc.ps1       SYSTEM worker on each machine (mounts, steps as system / as a user)
    guest/agent.ps1        logon agent: owns COM1, dot-sources lab.ps1 from the control disc
    guest/setup.ps1        first-logon setup of a base image (build phase)
    guest/autounattend*.xml.tmpl   unattended Windows Setup answer files (BIOS / UEFI)
    build-image.sh         unattended Windows install -> images/<name>.qcow2
    launch-test.sh         separate single-VM tester (no domain; see the end of this file)
    download-isos.sh, setup-host.sh, lib/common.sh, lib/*.py, lib/rename-isos.sh
                           host scripts and Python helpers of build-image.sh / launch-test.sh
    lib/oslist.conf        OS table (read by both the Rust and the bash side)
    images-list.conf       which ISO file serves which OS key (optional; else found by build)
    tests/                 all.sh (every no-VM test), ps_lint.py (PowerShell 4 lint),
                           run-tests.sh + fake-qemu (build-image.sh / launch-test.sh suite),
                           check-labs.sh, check-catalog.sh

Host requirements: `qemu-system-x86_64`, `qemu-img`, `/dev/kvm` (Linux), OVMF for UEFI
guests, and bash + python3 (stdlib only) for installing base images. No root, no Samba, no
xorriso/genisoimage: discs are written by the program itself.

## Settings (environment)

| variable | default | meaning |
|----------|---------|---------|
| `WTL_ROOT` | found from the binary's location (or the cwd) | the win-test-lab folder |
| `WTL_ISO_DIR` | `<root>/iso-files` | installer ISOs |
| `WTL_IMAGES_DIR` | `<root>/images` | base images `<os>-dc.qcow2`, `<os>-client.qcow2` |
| `WTL_QEMU`, `WTL_QEMU_IMG` | `qemu-system-x86_64`, `qemu-img` | the QEMU programs |
| `WTL_ACCEL` | kvm / whpx / hvf by host | `kvm`, `whpx`, `hvf` or `tcg`; `WTL_NO_KVM=1` = tcg |
| `WTL_CPU` | `host,hv_*` on KVM, `max` otherwise | the `-cpu` value |
| `WTL_CPUS` | 2 | vCPUs when a machine sets none |
| `WTL_NIC` | `e1000` | NIC model |
| `WTL_QMP` | `unix` (`tcp` on Windows) | QEMU monitor transport |
| `WTL_OVMF_CODE`, `WTL_OVMF_VARS` | `/usr/share/OVMF/OVMF_{CODE,VARS}_4M.fd` | UEFI firmware |
| `WTL_DISPLAY` | `gtk` (`cocoa` on macOS) | display used by `--show` |
| `WTL_PASSWORD` | `WinTest!2026` | local Administrator password baked into base images |
| `WTL_TEST_FAST=1` | | skip fixed settle sleeps (fake-QEMU tests only) |

## Base images

One unattended Windows install per OS and role: `images/<os>-dc.qcow2` for domain
controllers and `images/<os>-client.qcow2` for every other machine (members, member servers,
workgroup machines). A clone of a DC cannot join its own domain, so the two are separate
installs; machines of the same OS and role share one image, in any lab. Any Server OS key
works as a DC and any OS key as the rest (a 2012 DC + 2019 member = `2012-dc` + `2019-client`).
`lab` runs `build-image.sh <os> --name <os>-dc|<os>-client --computer-name DC<os>|CL<os>
--no-updates` for every missing image of a lab, in parallel, logging to
`labs/state/<lab>/build-<image>.log`.

build-image.sh: renders `guest/autounattend*.xml.tmpl`, puts it with `guest/*.ps1` and a
`WTL/build.json` on an `WTL_BUILD` disc, boots the installer ISO + that disc, waits for
`WTL BUILD_DONE` on the serial log and for the guest to power off, then renames
`<image>.qcow2.partial` to `<image>.qcow2` and writes `<image>.json`. In the guest,
`setup.ps1` runs at first logon: autologon, firewall off, power settings, optional Windows
Update rounds, then installs the agent autostart (HKLM Run `WTL-Agent` -> `C:\WTL\agent.ps1`)
**after** sending `BUILD_DONE` (an agent started earlier would hold COM1), and shuts down.

## Lab modes and names

A lab with one `role: "dc"` machine is an **AD lab**: that machine becomes the DC of a new
forest, the others join it, `users` are domain users. A lab without one is a **workgroup**:
no domain, `users` are local accounts on every machine. One workgroup machine needs no lab
LAN (it gets only QEMU user-mode NAT); every other lab has the multicast LAN below.

The run file's `lab` is a name (`labs/<name>.json` in the checkout) or a path relative to the
run file. The lab's **state name** (`labs/state/<name>/`, VM names `lab-<name>-<machine>`) is
the lab's `name` field, else the file name for `labs/*.json`, else the file name plus an
FNV-1a hash of the lab file's absolute path (`lab-3f9a1c2b`), so labs from different
projects never collide.

## The 0 state

`labs/state/<lab>/<machine>-0.qcow2`: a qcow2 overlay per machine on its base image. It is
built once (`lab prepare`, or the first `lab run`) by booting the overlays themselves:

1. Each machine gets a control disc (`<machine>.iso`, label `WTL_LAB`, drive D:) with
   `LAB\lab.ps1`, `LAB\labsvc.ps1` and `LAB\lab.json` (below). The agent finds it at logon
   and dot-sources `lab.ps1`.
2. AD, the DC: set the local Administrator password (`admin_password`; it becomes the domain
   Administrator's); rename to the machine name + reboot; static IP; AD DS + new forest +
   reboot; create the groups and users; `LAB_DC_READY`; register the SYSTEM worker.
3. AD, the members (in parallel): static IP with the DC as DNS; register the SYSTEM worker;
   set the local Administrator password; wait for the DC; `Add-Computer -NewName <machine>`
   (join + rename, one reboot, retried for 20 min while the new DC finishes its first boot);
   autologon as the machine's `logon` user (else the domain Administrator); `LAB_JOINED`;
   reboot; logged on -> `LAB_CLIENT_READY`; `LAB_IDLE`.
3. Workgroup machines (in parallel): static IP on the lab LAN if there is one; register the
   SYSTEM worker; set the local Administrator password; create the lab's users as local
   accounts in their local groups (missing groups are created); rename to the machine name;
   autologon as the machine's `logon` user (else Administrator); `LAB_JOINED`; reboot; logged
   on -> `LAB_CLIENT_READY`; `LAB_IDLE`.
4. The host powers every machine off with ACPI (members first, then the DC; a second press
   after 60 s if a user session delays it) and writes `0-state.json`.

The guest keeps its progress in `C:\WTL\lab-state.json` (`phase`: new, promoted, ready,
joined), so every reboot continues where it stopped. `lab rebuild` deletes the `-0.qcow2`
files and `0-state.json`; the base images stay.

## A run

`lab run CONFIG`: restore (QMP quit, delete `<machine>.qcow2`), create new throwaway
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
`lab up` is the same without steps.

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
 "run": null | {
   "mounts": [{"host": "/abs/host/path", "letter": "X"}],
   "steps": [{"n": 1, "as_": "system|logon|user", "user": "", "password": "",
              "path": "X:\\setup.bat", "args": "", "keep_running": false, "wait": 120}],
   "wait": 120 }
}
```

In workgroup mode `domain`, `netbios` and `dc_ip` are empty; `lan` is false (and `ip`
empty) for a machine without a lab LAN. `run` is set only for `lab run`, and only on
machines that have steps. `as_` is resolved per machine: a step `as` the machine's own logon
user, or with no `as`, is `logon` (its desktop session); another lab user is `user` with that
user's password (started by the SYSTEM worker with `LogonUser` + `CreateProcessAsUser`, as
`NETBIOS\user` in AD mode and `.\user` in a workgroup; no desktop); `system` is the SYSTEM
worker. lab.ps1 refuses a disc whose `fmt` is not `multi`.

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

COM1, a file on the host (`labs/state/<lab>/<machine>.serial.log`), text lines:

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
build: changes to it need `lab rebuild`.

## The single-VM tester (launch-test.sh)

Separate from the lab and older: one throwaway copy of a base image, no domain.

    ./launch-test.sh program.zip program.exe 2019 [--port 5000] [--test check.py] [--args ".."] [--expect-exit N]

It writes a `WTL_JOB` disc with `JOB.JSON` + `PAYLOAD.ZIP`; `agent.ps1` copies the zip to the
Desktop, extracts it, starts the program and reports `JOB_FOUND`, `COPIED`, `EXTRACTED`,
`LAUNCHED`, `READY port=P` (server mode, once the port listens) or `OUTPUT` + `PROC_EXIT
code=N secs=S` / `PROC_TIMEOUT` (exit mode, without `--port`). The host then runs the test
script with `WTL_HOST`, `WTL_PORT`, `WTL_OS`, `WTL_RESULTS`, `WTL_RUN_ID` (exit mode:
`WTL_EXIT_CODE`, `WTL_STDOUT`, `WTL_STDERR`) and writes `results/<run>/result.json`,
`test.log`, `serial.log`, `screen.png`. Exit 0 pass, 1 test failed, 2 setup error, 3 timeout.
It uses lib/common.sh and lib/{mkiso,qmp,serialwait,outputs}.py; `examples/demo`,
`examples/python-test.py`, `examples/fetch-ria` and `examples/fetch-test.py` are its examples.
