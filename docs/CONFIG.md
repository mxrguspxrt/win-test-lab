# Run configs

A **run config** describes one launch of a lab: which Windows lab to boot, which host folders
to expose as drives, and which scripts to run (and as whom). It is a small `key=value` file.

    lab run myapp.conf

The same lab (its "0 state") can be launched by many configs; each run starts from that clean
0 state, so configs are cheap to add and runs don't contaminate each other.

## Format

```ini
# lines starting with # are comments; blank lines ignored
version = 2019                          # required: the lab to boot (labs/<version>.conf)
mount   = ./app:X, /srv/shared:Z        # host folder -> drive letter in the guest
run_as_admin   = X:\install.bat, X:\configure.ps1
run_as_student = X:\run.exe
run_as_admin   = X:\cleanup.bat         # steps run in the order written (see below)
name    = smoke                         # optional: label for the results folder
wait    = 60                            # optional: seconds a step may run before it counts
                                        #           as "still running" (then it is left running)
```

### `version`
The lab to boot - the name of a `labs/<version>.conf` (e.g. `2012`, `2016`, `2019`). That lab
file defines the domain, the computer names, the network and the accounts.

### `mount`
Comma-separated `HOSTPATH:LETTER` pairs. Each host folder becomes a drive in the guest with
exactly that letter. Relative host paths are resolved **relative to the config file**.
- Letters are single `A`-`Z`, upper/lower doesn't matter, must be unique.
- `C:` (system) and `D:` (the lab's control disc) are reserved - use `X`, `Y`, `Z`, ...
- At most 3 mounts (the control disc takes one of the 4 virtual CD slots).
- **Mounts are read-only**, and are a *snapshot taken at launch* - edits the guest makes do
  not flow back, and edits you make on the host after launch are not seen until the next run.
  (This is how we expose folders without needing Samba or root on the host. A writable share is
  a possible future addition.)

### `run_as_admin` / `run_as_student`
Comma-separated scripts/executables to run from the mounted drives (or `C:`).
- `run_as_admin` steps run with **full privilege** (as the SYSTEM account - the lab's "root").
- `run_as_student` steps run as the ordinary domain user **`LAB\student`**.
- **Steps run in the order they appear in the config.** `run_as_admin` and `run_as_student`
  lines may be freely interleaved, so you can e.g. install as admin, run as the student, then
  clean up as admin. A single line's comma-separated scripts run left to right. Both keys may
  appear any number of times.
- Each must start with a mounted drive letter (or `C:`) and end in `.bat`, `.cmd`, `.ps1` or
  `.exe`. At least one step is required.

Example - install, run, clean up:

```ini
version = 2019
mount   = ./app:X
run_as_admin   = X:\install.bat     # 1. install it (as root)
run_as_student = X:\app.exe         # 2. run it as the student
run_as_admin   = X:\uninstall.bat   # 3. clean up (as root)
```

Each step's stdout/stderr and exit code are captured and reported. `lab run` exits non-zero if
any step failed (non-zero exit) or timed out.

## Results

Each run writes to `labs/state/<version>/runs/<timestamp>/`:
- `step1out.txt`, `step1err.txt`, ... - each step's output
- `screen.png` - the student's desktop at the end
- `client.serial.log` - the full guest event log
- `config.conf` - a copy of the config that was run

## Examples

See `examples/configs/`:
- `2019-smoke.conf` - mount the smoke-test example, install it as admin, run it as the student.
- `2019-student-only.conf` - same 0 state, student step only (no admin step).
