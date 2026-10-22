# win-test-lab

Test your Windows software on Windows Server 2012 R2 / 2016 / 2019 / 2022 / 2025 the easy way:
spin up a throwaway VM - or a tiny Active Directory domain (a domain controller + a
domain-joined client logged in as an ordinary user) - drop your program in, run it, see if it
works, reset to a clean state, repeat.

Runs on a stock Linux machine with QEMU/KVM. No Samba, no root required, Python standard
library only. Windows evaluation ISOs are free from Microsoft (see [docs/ISOS.md](docs/ISOS.md)).

```
lab run myapp.conf                 # boot a clean AD lab, mount folders, run scripts as admin / as the student
lab run myapp.conf win11.conf      # layer configs: workload from one, the OS from another
lab restore 2019                   # throw away everything since the clean "0 state" (~1 s)
```

- **The AD lab** (`lab.sh`, installed as `lab`): domain controller + student client, run configs,
  reset-to-clean. See [AD lab](#ad-lab-domain-controller--student-machine-lab) below and
  [docs/CONFIG.md](docs/CONFIG.md).
- **The single-VM tester** (`launch-test.sh`): one VM, no domain, run a program and test it.
- **Docs:** [docs/ISOS.md](docs/ISOS.md) (get the ISOs), [docs/CONFIG.md](docs/CONFIG.md) (run
  configs), [docs/AGENTS.md](docs/AGENTS.md) (architecture / for AI agents), [SPEC.md](SPEC.md)
  (internal contract).
- **Tests:** `tests/all.sh` (no VM needed).

## One-time setup (on your Linux machine)

    cd win-test-lab
    ./setup-host.sh                 # installs qemu, checks /dev/kvm (log out/in if added to kvm group)
    # download the Windows evaluation ISOs you want into iso-files/ - see docs/ISOS.md
    ./download-isos.sh 2016         # tries to fetch the 2016 eval ISO (~6 GB) into iso-files/

## Build base images (once per OS, unattended, includes Windows Update)

    ./build-image.sh 2012 2016      # several hours (Windows Update is slow, esp. 2012 R2)
    ./build-image.sh 2012 --show    # same, but shows the VM window so you can watch
    ./build-image.sh 2016 --no-updates   # quick build without updates (for trying things out)

Result: images/2012.qcow2, images/2016.qcow2 (~20-30 GB each, grows-as-needed disk).

## Run a test

    ./launch-test.sh program.zip program.exe 2012 --port 5000 --test python-test.py
    ./launch-test.sh program.zip program.exe 2016 --port 5000 --test python-test.py
    ./launch-test.sh program.zip program.exe all  --port 5000 --test python-test.py

What happens: a throwaway copy of the image boots, Administrator logs in automatically,
program.zip is copied to the Desktop and extracted, program.exe starts, the host waits
until guest port 5000 is listening, then runs python-test.py. Port 5000 in the VM is
reachable at 127.0.0.1:5000 on the host (use `--port 5000:15000` to pick another host port).

Your test script gets env vars: WTL_HOST, WTL_PORT, WTL_OS, WTL_RESULTS, WTL_RUN_ID.
See examples/python-test.py.

Results land in results/<date>-<os>/: result.json, test.log, serial.log, screen.png.
Exit code: 0 pass, 1 test failed, 2 setup/launch error, 3 timeout.

Useful options: `--show` (watch the VM), `--keep` (leave VM running to poke around),
`--args "..."` (program arguments), `--timeout 900`.

## Programs that just run and exit (no port)

Leave out `--port` and the program runs until it exits on its own. Its stdout,
stderr and exit code are saved, and your test script checks them afterwards:

    ./launch-test.sh program.zip program.exe 2012 --test my-check.py
    ./launch-test.sh program.zip program.exe 2012                     # pass = exit code 0
    ./launch-test.sh program.zip program.exe 2012 --expect-exit 3     # pass = exit code 3

The test script gets WTL_EXIT_CODE, WTL_STDOUT (file), WTL_STDERR (file) as well as the
usual WTL_OS / WTL_RESULTS. The program is killed after `--run-timeout` (default 300 s).
results/<run>/ then also holds stdout.txt and stderr.txt. result.json names common crash
codes (e.g. 0xC0000135 = missing DLL, 0xC0000139 = needs a newer Windows).
The VM has internet through the host (NAT), so programs can download things.

### Example: Rust program that fetches the ria.ee front page

    sudo apt-get install gcc-mingw-w64-x86-64          # once; plus Rust from https://rustup.rs
    examples/fetch-ria/build.sh                        # -> examples/fetch-ria/fetch-ria.zip
    ./launch-test.sh examples/fetch-ria/fetch-ria.zip fetch-ria.exe 2012 --test examples/fetch-test.py

fetch-ria uses Windows' own HTTPS stack (WinHTTP/Schannel) and prints status, size and
page title. fetch-test.py checks: exit 0, HTTP 200, >1000 bytes, a title, "RESULT OK".

## Try the demo first

    examples/make-demo-zip.sh
    ./launch-test.sh examples/demo/program.zip program.bat 2012 --port 5000 --test examples/python-test.py

## AD lab: domain controller + student machine (`lab`)

A run is launched by a **config file**: it mounts host folders as drives and runs your scripts
as admin / as the student.

    lab run myapp.conf
    lab run myapp.conf                         # run again: same command, starts from a clean 0 state
    lab run myapp.conf win11.conf              # layer configs: workload from one, the OS from another

    lab restore 2012                           # VMs off, everything since the 0 state thrown away
    lab status 2012

`lab` is `win-test-lab/lab.sh` (link it into your PATH: `ln -s "$PWD/lab.sh" ~/.local/bin/lab`).
Labs: `labs/2012.conf`, `labs/2016.conf`, `labs/2019.conf`.

### Config runs (mounts, admin vs student)

A **config** is a small `key=value` file - full reference in [docs/CONFIG.md](docs/CONFIG.md):

```ini
version = 2019                      # which lab (labs/2019.conf) to boot
mount   = ./app:X, /srv/data:Z      # host folder -> drive letter in the guest (read-only)
run_as_admin   = X:\install.bat     # runs with full privilege (SYSTEM = the lab's root)
run_as_student = X:\run.exe          # runs as the ordinary domain user LAB\student
run_as_admin   = X:\cleanup.bat      # ...and so on, in the order written
```

- The **same 0 state** is launched by any number of configs; keep several (e.g. one with an
  admin install step, one student-only) and switch freely - each run starts clean.
- `mount` exposes a host folder as a fixed drive letter. It is **read-only** and a snapshot
  taken at launch (this is how it works without Samba or root).
- `run_as_admin` steps run as the SYSTEM account (root-equivalent); `run_as_student` steps run
  as `LAB\student`. **Steps run in the order they appear** in the config, so `run_as_admin` and
  `run_as_student` lines can be interleaved (e.g. install as admin, run as the student, clean up
  as admin). Each step's output and exit code are captured to `labs/state/<lab>/runs/<time>/`;
  the run exits non-zero if any step failed.

Try it: `examples/smoke-test/build.sh && lab run examples/configs/2019-smoke.conf`.

**The 0 state** = Windows installed, domain `lab.local` created, client joined, `LAB\student`
logging on automatically, both shut down cleanly. It is built once (the first `lab run`, or
`lab prepare 2012`; ~25 min) and frozen in `labs/state/2012/*-0.qcow2`. Every run writes only
to small throwaway disks on top of it, so `restore` and the restore at the start of every
`run` take a second, and a run never changes the 0 state. `lab rebuild 2012` deletes the 0
state (the next run makes a new domain; the Windows installs are kept).

### What a run does

Boots both VMs from the 0 state (~2 min), mounts the config's folders as read-only drives,
then runs the config's steps in order. Each `run_as_admin` step runs as the SYSTEM account
(the lab's root); each `run_as_student` step runs as `LAB\student`. A step that is still
running after `--wait` seconds (default 60, or the config's own `wait`) is left running (for
GUI apps and servers). Each step's stdout, stderr and exit code are captured; `lab run` exits
non-zero if any step failed. The VMs keep running after the run so you can look (VNC addresses
are printed); results incl. a screenshot are in `labs/state/<lab>/runs/<time>/`.

Logins: `LAB\student` / `Pupil!2026`, `LAB\labadmin` / `DomAdm!2026` (Domain Admins),
`LAB\Administrator` / `WinTest!2026`. DC 10.77.12.10, client 10.77.12.20 (2019: 10.77.19.x,
2016: 10.77.16.x). The two VMs share a private network (the DC is DNS and has no internet);
the client also has internet. Each VM has its own Windows install (images/2012-dc.qcow2,
images/2012-client.qcow2), because a domain refuses to join a copy of its own DC.

Another Windows version: copy `labs/2019.conf` to e.g. `labs/2022.conf`, set `LAB_OS=2022`,
new computer names, `LAB_NET` and `LAB_LAN_PORT`; then point a config at it (`version = 2022`).
Labs can run at the same time (different networks and ports). `--show` opens the VM windows.

## Self-test (no VM needed)

    tests/all.sh          # bash syntax + PowerShell lint + config parser + fake-qemu suite
    tests/run-tests.sh    # just the fake-qemu host-logic suite
    tests/test_labconf.py # just the run-config parser unit tests
