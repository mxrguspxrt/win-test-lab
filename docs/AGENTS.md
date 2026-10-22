# Guidance for AI coding agents working on win-test-lab

This file orients an LLM (Claude Code, etc.) working in this repo. Humans: this doubles as an
architecture overview.

## What this project is

A dependency-light harness to install Windows Server in throwaway QEMU/KVM VMs, stand up a tiny
Active Directory domain (a domain controller + a domain-joined client that auto-logs-in as
`LAB\student`), and run a user's program/scripts on the client - then throw it all away and do
it again. It exists so people can test their software on several Windows versions easily.

Two entry points:
- `lab.sh` (installed as `lab`) - the AD lab: `lab run <config> [<config>...]`, plus `restore`,
  `up`, `stop`, `status`, `prepare`, `rebuild`. A run is always driven by a config file (mounts +
  `run_as_admin` / `run_as_student` steps, run in the order written).
- `launch-test.sh` - the older single-VM (no domain) program tester (zip + exe, the JOB.JSON /
  `guest/agent.ps1` serial protocol in SPEC.md). Still supported, separate from `lab.sh`.

## Hard constraints (do not break these)

- **Host:** Linux + `qemu-system-x86_64` + `qemu-img` + `python3` (3.8+ stdlib only). No root
  is assumed, and **no Samba**. Do not add a host dependency without a very good reason and a
  fallback; the whole point is that it runs on a stock machine. ISOs are written by the
  pure-python `lib/mkiso.py` (no xorriso/genisoimage).
- **Guest PowerShell must be 4.0 / .NET 4.5 compatible** (Server 2012 R2 is the floor). No
  `Expand-Archive`, no `::new()`, no classes, no `-AsHashtable`. `tests/ps_lint.py` enforces a
  subset - run it after editing any `guest/*.ps1`.
- **Serial protocol:** the guest talks to the host over COM1 with `WTL <EVENT> k=v | text`
  lines (see `SPEC.md`). Values with spaces are URL-escaped. Only ONE process writes COM1 at a
  time (the agent); the privileged worker talks via files, not serial.

## How a lab works (the moving parts)

1. **Base install** (`build-image.sh`): unattended Windows install from the ISO via
   `guest/autounattend.xml.tmpl` -> `images/<lab>-dc.qcow2`, `images/<lab>-client.qcow2`.
   `guest/setup.ps1` runs at first logon (autologon, firewall off, optional Windows Update).
2. **0 state** (`lab prepare`): boots DC + client once; `guest/lab.ps1` promotes the DC to a new
   forest, creates users, joins the client, sets the student autologon, registers the SYSTEM
   worker, then both shut down. The resulting disks are frozen as `labs/state/<lab>/*-0.qcow2`.
   DC and client get **separate** installs because a clone of the DC can't join its own domain.
3. **A run** (`lab run`): makes throwaway qcow2 overlays on the 0-state disks, boots both,
   mounts the config's folders (read-only ISOs, labelled `WTL<LETTER>`), runs the steps, reports.
   `lab restore` / the next run just deletes the overlays - that is the "reset to 0" mechanism.

Privilege split on the client: the ordinary `LAB\student` session runs `guest/agent.ps1` (owns
COM1) and `guest/lab.ps1` (orchestration + student steps). A SYSTEM scheduled task runs
`guest/labsvc.ps1`, which does what the student can't - assign drive letters, run admin steps -
driven by request/response JSON files under `C:\WTL\svc`. This avoids juggling admin passwords
or rebooting between phases.

## Networking

Two VMs on a private QEMU `-netdev stream` segment over `127.0.0.1:<LAB_LAN_PORT>`; the DC is
the DNS server and has no internet, the client also has user-mode NAT. Static IPs from the lab
conf (`LAB_NET`). Different labs use different nets/ports, so several can run at once.

## Files

    lab.sh                 AD lab CLI
    build-image.sh         unattended Windows install -> base image
    launch-test.sh         single-VM program tester (no domain)
    labs/<v>.conf          lab definition (domain, names, net, accounts) - bash key=value
    examples/configs/*.conf  run configs (see docs/CONFIG.md) - the parsed key=value format
    lib/common.sh          shared bash: qemu cmdline, OS table, serial/QMP helpers
    lib/labconf.py         run-config parser/validator (unit-tested)
    lib/mkiso.py           pure-python ISO9660+Joliet writer
    lib/isoinfo.py         pure-python WIM/UDF reader (identify an ISO)
    lib/qmp.py             QEMU monitor client (screenshot, powerdown, quit)
    lib/serialwait.py      wait for WTL events in a serial log
    lib/outputs.py         decode captured program/step output from the serial log
    guest/autounattend.xml.tmpl   unattended answer file
    guest/setup.ps1        build phase (first logon)
    guest/lab.ps1          AD lab orchestration (DC promote, join, mounts, run steps)
    guest/labsvc.ps1       SYSTEM worker (privileged mounts + admin steps)
    guest/agent.ps1        single-VM test agent (owns COM1)
    tests/all.sh           run all no-VM tests
    tests/test_labconf.py  run-config parser unit tests
    tests/run-tests.sh     fake-qemu host-logic suite
    tests/ps_lint.py       PowerShell 4.0 compatibility linter

## When you change things

- Editing `guest/lab.ps1` or `guest/labsvc.ps1`: they ship on the lab control CD and are copied
  fresh each boot, so a plain `lab run` picks them up **except** anything baked at 0-state build
  time (the SYSTEM task registration, the join, autologon). If you touch those, `lab rebuild
  <lab>` then `lab run` to re-bake.
- Editing `guest/setup.ps1` / `autounattend.xml.tmpl`: these live in the base image - rebuild
  it (`build-image.sh --force`, or delete `images/<lab>-*.qcow2`), which is slow.
- Always run `tests/all.sh` before claiming something works. It needs no VM.
- Keep the serial protocol and the `WTL_*` env contract stable; `SPEC.md` is the source of truth.
