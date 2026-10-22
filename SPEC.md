# win-test-lab — internal contract (SPEC)

Goal: on a Linux host with QEMU/KVM, build fully-updated Windows Server base images
unattended, then per test run:

    ./launch-test.sh program.zip program.exe 2012 --port 5000 --test python-test.py

boots a throwaway copy of the 2012 image, Administrator auto-logs in, the guest agent
copies program.zip to the Desktop, extracts it, starts program.exe, reports over the
serial port, then the host runs python-test.py against forwarded ports and collects results.

No component may assume internet inside the build sandbox. Host tools available on the
user's machine after `setup-host.sh`: bash, python3 (3.8+ stdlib only), qemu-system-x86_64,
qemu-img. NO xorriso/genisoimage/7z dependency — ISOs are written by `lib/mkiso.py`.

## Layout (root = win-test-lab/)

    setup-host.sh          apt install qemu-system-x86 qemu-utils python3; check /dev/kvm + kvm group
    download-isos.sh       fetch missing eval ISOs into $WTL_ISO_DIR (best effort, fwlinks)
    build-image.sh <os>... unattended install + Windows Update -> images/<os>.qcow2
    launch-test.sh         run one test (see CLI)
    lib/common.sh          shared bash: config load, logging, os lookup, qemu cmdline builder
    lib/oslist.conf        OS table (see below)
    lib/mkiso.py           pure-python ISO9660 + Joliet writer
    lib/isoinfo.py         pure-python UDF reader: list files, read sources/install.wim XML, pick image index
    lib/qmp.py             QMP client: screendump (writes PNG), system_powerdown, quit
    lib/serialwait.py      wait for WTL events in a serial log file
    guest/autounattend.xml.tmpl
    guest/setup.ps1        build phase (runs at first logon, survives reboots)
    guest/agent.ps1        test phase agent (runs at every logon)
    examples/demo/         program.bat + echo-server.ps1 (zip-able demo "program")
    examples/python-test.py
    tests/                 fake-qemu, fake-qemu-img, run-tests.sh (host logic tests w/o KVM)
    images/                base images <os>.qcow2 (+ <os>.json metadata)
    runs/                  per-run overlays (deleted after run unless --keep)
    results/<YYYYmmdd-HHMMSS>-<os>/   per-run outputs

## Environment overrides (all optional)

    WTL_ISO_DIR      default: parent dir of win-test-lab (the user's "ISO images" folder)
    WTL_IMAGES_DIR   default: <root>/images
    WTL_QEMU         default: qemu-system-x86_64
    WTL_QEMU_IMG     default: qemu-img
    WTL_NO_KVM=1     omit -enable-kvm/-cpu host (tests set this)
    WTL_MEM          default 4096 (MB)      WTL_CPUS default 2
    WTL_PASSWORD     default "WinTest!2026" (Administrator)

## lib/oslist.conf

Whitespace-separated, `#` comments. Columns:

    key  iso_glob                                   edition_id      inst_type  fallback_index  min_disk_gb  display
    2012 *9600*SERVER_EVAL*.[iI][sS][oO]          ServerStandard  Server     2   60  Windows Server 2012 R2
    2016 *14393*SERVER_EVAL*.[iI][sS][oO]         ServerStandard  Server     2   60  Windows Server 2016
    2019 *17763*SERVER_EVAL*.[iI][sS][oO]         ServerStandard  Server     2   60  Windows Server 2019
    2022 *20348*SERVER_EVAL*.[iI][sS][oO]         ServerStandard  Server     2   60  Windows Server 2022
    2025 *26100*SERVER_EVAL*.[iI][sS][oO]         ServerStandard  Server     2   80  Windows Server 2025

Aliases accepted on CLI: 2012r2 -> 2012. `all` = every key whose base image exists (launch-test)
or whose ISO exists (build-image). If several ISOs match a glob, pick the newest mtime;
`--iso PATH` overrides. Note: existing ISO "2022server.iso" doesn't match 20348 glob — the
lookup must also fall back to isoinfo (read WIM XML <BUILD>) over all *.iso in WTL_ISO_DIR
when the glob matches nothing. (2012 R2 build = 9600, 2016 = 14393, 2019 = 17763,
2022 = 20348, 2025 = 26100.) Skip ISOs that have no sources/install.wim (e.g. LOF/FOD disc).

## lib/isoinfo.py CLI

    isoinfo.py ls ISO [PATH]                 list dir (UDF; fall back to ISO9660)
    isoinfo.py wim ISO                       JSON list of images: index,name,edition_id,installation_type,build,spbuild,arch
    isoinfo.py pick ISO EDITION INST_TYPE    print index of matching image, exit 1 if none
    isoinfo.py build ISO                     print major build of image 1 (e.g. 9600)

Windows ISOs are UDF-bridge: the ISO9660 tree only has README.TXT; real tree is UDF
(2.01/2.50, may use metadata partition on newer media). install.wim may be install.esd on
some media (ESD header is also "MSWIM\0\0\0"; XML location same header field).

## lib/mkiso.py CLI

    mkiso.py -o out.iso -V VOLID  SRC_DIR

Writes ISO9660 (level 2 names, uppercase) + Joliet (long names, original case) with
directories. Must be readable by Windows Setup (autounattend.xml at root is found by
Setup on any removable/CD media, name is case-insensitive) and by isoinfo.py.

## Build phase (build-image.sh <os> [--iso PATH] [--no-updates] [--disk-gb N] [--show] [--timeout-h H] [--force])

1. Create images/<os>.qcow2.partial (qcow2, min_disk_gb).
2. Stage dir -> mkiso.py -> runs/build-<os>/unattend.iso (vol id WTL_BUILD), contents:
       autounattend.xml            (template rendered: @IMAGE_INDEX@ @PASSWORD@ @COMPUTERNAME@ = WTL-<OS>)
       WTL/setup.ps1  WTL/agent.ps1  WTL/build.json  {"os":"2012","updates":true,"password":"..."}
3. QEMU boots: install ISO + unattend ISO, `-boot once=d`, serial to runs/build-<os>/serial.log.
4. Host waits (serialwait) for `WTL BUILD_DONE` and then for qemu process exit (guest shuts
   itself down). Progress events echoed to the terminal. On `WTL BUILD_FAIL` or timeout
   (default 12h): screenshot, kill, keep .partial, exit non-zero.
5. Rename .partial -> images/<os>.qcow2, write images/<os>.json {os, iso, built_at, updates}.
   `qemu-img` is never required to inspect it later.

Guest side (setup.ps1, PowerShell 4.0 compatible — Server 2012 R2 baseline):
  first run (FirstLogonCommands finds X:\WTL\setup.ps1 on any CD drive):
  copies WTL\* to C:\WTL, sets permanent autologon (Winlogon registry AutoAdminLogon=1,
  DefaultUserName=Administrator, DefaultPassword), disables firewall (all profiles),
  disables Server Manager at logon, IE ESC off, power plan high performance, never sleep/
  screen-off, disables Windows Update automatic install & reboots (AUOptions=1 / NoAutoUpdate),
  disables "Shutdown Event Tracker", sets HKLM Run key `WTL-Build` -> setup.ps1 (resume
  after reboot). Then Windows Update loop using Microsoft.Update.Session COM:
  search "IsInstalled=0 and Type='Software' and IsHidden=0", accept EULAs, download, install,
  report `WTL WU_ROUND n found=x installed=y reboot=true|false` ; reboot if needed (run key
  resumes); stop when 0 found twice in a row or 30 rounds. Errors per round logged, not fatal,
  except 3 consecutive failed rounds -> continue to finish anyway with WU_GAVEUP event.
  finish: remove Run `WTL-Build`, add Run `WTL-Agent` ->
  `powershell.exe -NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File C:\WTL\agent.ps1`,
  cleanup (Dism /Online /Cleanup-Image /StartComponentCleanup best effort, clear temp),
  emit `WTL BUILD_DONE`, `Stop-Computer -Force`.

## Test phase (launch-test.sh)

    launch-test.sh ZIP EXE OS [options]
      ZIP           path to zip on host
      EXE           file name or relative path inside the zip (e.g. program.exe or bin/program.exe)
      OS            key / alias / "all" (runs sequentially, summary table at end)
    options:
      --test FILE       host-side test script (python3 FILE; .sh run with bash). Optional.
      --port P[:H]      forward guest TCP port P to host port H (default H=P). Repeatable.
      --args "..."      command-line args for EXE
      --wait-port P     agent waits until guest port P is listening before reporting READY (default: first --port)
      --timeout S       overall seconds before failure (default 900)
      --show            show VM window (gtk). Default headless (-display none) + VNC on 127.0.0.1:59xx printed
      --keep            keep VM running after test (prints how to connect) and keep overlay
      --test-arg A      extra arg passed to test script (repeatable)

Steps:
1. Validate zip/exe(name present in zip via python zipfile), base image exists.
2. Overlay: runs/<run>/disk.qcow2 backed by images/<os>.qcow2 (`qemu-img create -f qcow2 -F qcow2 -b`).
3. Job dir -> mkiso.py -> runs/<run>/job.iso (vol id WTL_JOB) containing:
       JOB.JSON     {"run_id","zip_name","exe","args","wait_ports":[...],"timeout"}
       PAYLOAD.ZIP  (copy of ZIP)
4. QEMU: disk on AHCI, install media absent, job.iso as CD, e1000 user net with
   hostfwd=tcp:127.0.0.1:H-:P for each port, serial file runs/<run>/serial.log,
   QMP unix socket runs/<run>/qmp.sock.
5. Wait for events: AGENT_START (boot timeout), then LAUNCHED, then READY (port listening)
   or LAUNCH_FAIL/ERROR -> fail. PROC_EXIT before READY -> fail.
6. Run test script with env:
       WTL_HOST=127.0.0.1  WTL_PORT=<host port of first --port>  WTL_PORT_<P>=<H> for each
       WTL_OS=<key>  WTL_RESULTS=<results dir>  WTL_RUN_ID
   stdout/stderr tee'd to results/.../test.log. Exit code recorded.
7. Screenshot (qmp screendump -> screen.png), copy serial.log, write result.json
   {os, run_id, status: pass|fail|error|timeout, test_exit, durations, events}.
   Power off (qmp quit), delete overlay unless --keep.
Exit: 0 pass; 1 test failed; 2 setup/launch error; 3 timeout. For `all`: worst code.

## Serial protocol (guest -> host, COM1, 115200 8N1, text lines, CRLF ok)

    WTL <EVENT> [key=value ...] [| free text]
Values with spaces must be URL-escaped (%20). Host ignores non-WTL lines (still logged).
Events: AGENT_START os=<caption> build=<n>; JOB_WAIT; JOB_FOUND drive=D:; COPIED path=...;
EXTRACTED dir=...; LAUNCHED pid=N exe=...; READY port=P; PROC_EXIT code=N;
LOG | text; ERROR | text; LAUNCH_FAIL | text; WU_ROUND ...; WU_GAVEUP; BUILD_DONE; BUILD_FAIL | text.

## agent.ps1 (PowerShell 4.0, .NET 4.5 — must not use PS5+ features: no Expand-Archive,
no class keyword, no ::new(), no -AsHashtable, no Get-CimInstance-only paths that PS4 lacks is fine)
- Opens COM1 via System.IO.Ports.SerialPort (fallback: log only), logs also C:\WTL\agent.log.
- Emits AGENT_START. Polls up to 600s every 2s all drives (DriveType CD-ROM) for \JOB.JSON.
  None -> LOG | no job, exit quietly (normal for manual boots).
- Copies PAYLOAD.ZIP to "$env:USERPROFILE\Desktop\<zip_name>", extracts with
  [System.IO.Compression.ZipFile]::ExtractToDirectory into Desktop\<zip stem>\ (remove existing first).
- Resolves EXE: relative path inside extract dir; else first recursive match by file name.
- Start-Process -PassThru -WorkingDirectory <exe dir> [-ArgumentList args]. .bat/.cmd/.ps1 ok
  (.ps1 via powershell -ExecutionPolicy Bypass -File).
- LAUNCHED; then for each wait_port poll TcpClient 127.0.0.1:P until open (within timeout)
  -> READY port=P; if no wait_ports -> READY after 3s. If process exits first -> PROC_EXIT code.
- Afterwards keeps monitoring; emits PROC_EXIT when program exits; stays alive.

## Exit mode (added 2026-10-21)

JOB.JSON gets "mode": "exit"|"server" (launch-test default: exit when no --port).
In exit mode "timeout" = program run limit (--run-timeout). agent.ps1 Invoke-RunToExit runs
the EXE with stdout/stderr redirected to C:\WTL\run\*.txt (stdin = empty file), waits, then sends
`WTL OUTPUT stream=stdout|stderr seq=N | <base64 of <=768 bytes>` (cap 256 KiB per stream),
`WTL OUTPUT_END stream=.. bytes=.. sent=.. truncated=0|1`, then `WTL PROC_EXIT code=N secs=S`
or `WTL PROC_TIMEOUT secs=S` (tree killed). Error dialogs off (SetErrorMode 0x8003, WER DontShowUI).
Host: lib/outputs.py decodes into results/<run>/stdout.txt + stderr.txt; result.json gets
"mode", "program" {exit_code, exit_hex, exit_name, timed_out, ...}, "expect_exit".
Pass rule: test script exit 0 (and exit code == --expect-exit if given); with no --test:
exit code == --expect-exit (default 0). PROC_TIMEOUT -> status timeout, exit 3.

## AD lab (added 2026-10-22)

lab.sh builds a 2-VM Active Directory lab and runs programs/scripts on the client.

- labs/<v>.conf (bash key=value): LAB_OS/LAB_CLIENT_OS, LAB_DOMAIN/LAB_NETBIOS, LAB_DC_NAME/
  LAB_CLIENT_NAME, LAB_NET (DC=.10 client=.20), LAB_PREFIX, LAB_LAN_PORT, LAB_USER/_PASSWORD,
  LAB_ADMIN_USER/_PASSWORD, LAB_DC_MEM/LAB_CLIENT_MEM. Passwords must not contain the account name.
- Separate base installs images/<v>-dc.qcow2 + <v>-client.qcow2 (a clone of a DC can't join its
  own domain). build-image.sh gained --name NAME and --computer-name CN for this.
- Private VM-to-VM net: qemu_cmd --lan server:PORT|client:PORT (netdev stream on 127.0.0.1:PORT),
  --lan-mac MAC, --no-usernet (DC has no NAT; client keeps user NAT + optional --fwd).
- 0 state: lab prepare boots DC+client once; guest/lab.ps1 (dot-sourced by agent.ps1 when a CD
  has \LAB\lab.ps1 + \LAB\lab.json) promotes the DC (Install-ADDSForest), creates users, joins the
  client (Add-Computer, retries ~20 min for the DC's slow first boot), sets student autologon via
  the LSA secret DefaultPassword + registry, registers the SYSTEM task WTL-Svc, then LAB_IDLE and
  a clean ACPI power-off. Disks frozen as labs/state/<v>/{dc,client}-0.qcow2. Events: LAB_START,
  LAB_PHASE, LAB_REBOOT, LAB_DC_READY, LAB_JOINED, LAB_CLIENT_READY, LAB_IDLE, LAB_FAIL.
- A run: throwaway overlays on the 0-state disks; lab restore / next run deletes them. A run is
  always driven by a config: `lab run CONFIG [CONFIG...] [--show --wait S --timeout-min M]`
  (lib/labconf.py parses examples/configs/*.conf, merged left->right, -> JSON). mounts + ordered
  steps. Each mount HOSTPATH:LETTER -> a read-only ISO labelled WTL<LETTER> (extra CD on the
  client), given its fixed drive letter in the guest. Mounts are a snapshot at launch (no
  Samba/root needed). steps is an ordered list of {who: admin|student, path}, built from the
  run_as_admin / run_as_student lines IN THE ORDER WRITTEN (the two keys may interleave). Steps:
  guest/lab.ps1 Invoke-LabRun walks the list in order; admin steps go to guest/labsvc.ps1 (SYSTEM
  worker, started by the WTL-Svc ONSTART task, request/response JSON under C:\WTL\svc) so no
  reboot/password-juggling; student steps run in the student session.
    Each step -> WTL LAB_STEP as=admin|student n=N script=.. exit=N|timeout|launchfail secs=S, with
    output as OUTPUT stream=stepNout|stepNerr. Ends with LAB_RUN_DONE admin=A student=S.
    lib/outputs.py decodes any stream name to <stream>.txt. Exit: 0 all steps ok, 1 a step failed.
  (The separate launch-test.sh single-VM tester - zip+exe, JOB.JSON, guest/agent.ps1 - is below.)
- Drive letters: C: system, D: the lab control disc; mounts use X/Y/Z (labconf rejects C/D and
  >3 mounts, since the client has 4 IDE CD slots and the control disc takes one).

Tests (no VM): tests/all.sh = bash -n + ps_lint + test_labconf.py + example-config validation +
run-tests.sh (fake-qemu). ps_lint.py enforces PowerShell 4.0; all guest/*.ps1 must pass.
