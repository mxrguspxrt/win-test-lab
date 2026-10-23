# Labs and run configs

Two small JSON files describe everything:

- a **lab**, `labs/<name>.json`: the domain, the private network, the machines and the users.
  It is built once into a **0 state** (Windows installed, domain created, members joined,
  their users logged on) and then reused by every run.
- a **run config**, any `<something>.json`: which lab to boot, which host folders to mount
  as drives, and which programs to run on which machine as whom.

```
lab run examples/configs/school-smoke.json
```

restores the lab to its 0 state, boots it, mounts the folders, runs the steps and reports
each one. The same lab is used by any number of run configs; every run starts clean.

Both files are strict: an unknown field, a misspelt machine or user name, a step on a drive
that is not mounted, two machines on one IP and the like are reported with the file name
**before anything boots**.

## Editor support (JSON Schema)

The schemas are in `wtl-rs/schema/lab.schema.json` and `wtl-rs/schema/run.schema.json`
(regenerate them with `wtl-rs schema` after changing the format). Point a file at its schema
with a `$schema` field and editors such as VS Code validate and autocomplete it:

```json
{ "$schema": "../wtl-rs/schema/lab.schema.json", ... }          // in labs/
{ "$schema": "../../wtl-rs/schema/run.schema.json", ... }       // in examples/configs/
```

`lab` ignores `$schema`. The path is relative to the JSON file itself.

## The lab file: `labs/<name>.json`

```json
{
  "domain": "lab.local",
  "netbios": "LAB",
  "admin_password": "WinTest!2026",
  "network": { "subnet": "10.77.30", "lan_port": 47030 },
  "machines": {
    "DC1": { "os": "2019", "role": "dc" },
    "CL1": { "os": "2012", "logon": "student1" },
    "CL2": { "os": "2016", "logon": "student2" }
  },
  "users": {
    "student1": { "password": "Pupil!2026", "groups": ["Students"] },
    "student2": { "password": "Pupil!2026", "groups": ["Students"] },
    "teacher":  { "password": "Teach!2026", "groups": ["Teachers"] }
  }
}
```

The lab's name is the file name: `labs/school.json` is the lab `school`, and its state lives
in `labs/state/school/`.

| field | required | meaning |
|-------|----------|---------|
| `domain` | yes | DNS name of the new AD forest, e.g. `lab.local`. |
| `netbios` | yes | NetBIOS domain name (1-15 of `A-Z 0-9 . _ -`); users log on as `LAB\student1`. |
| `admin_password` | yes | Password of the domain `Administrator` (also the DSRM password). Must meet Windows' complexity rules and must not contain the account name. |
| `network.subnet` | yes | First three octets of the lab's private /24, e.g. `10.77.30`. |
| `network.lan_port` | yes | UDP port of the lab's private LAN on the host (any free port; two labs that run at the same time need different ports). |
| `machines` | yes | The machines, keyed by **name**. The name is also the Windows computer name (1-15 of `A-Z 0-9 . _ -`). Exactly one machine has `role: "dc"`. |
| `users` | no | Domain users, keyed by user name. Default: none. |

A **machine**:

| field | default | meaning |
|-------|---------|---------|
| `os` | (required) | OS key from `lib/oslist.conf`: `2012`, `2016`, `2019`, `2022`, `2025`, `win10`, `win11` (aliases like `2012r2`, `ws2019` work too). The DC must be a Windows Server. |
| `role` | `"member"` | `"dc"` (the domain controller; exactly one) or `"member"` (joined to the domain). |
| `logon` | none | A user from `users` that this machine logs on as automatically. Members usually set one; the DC cannot. Steps run as this user by default. |
| `mem` | 3072 (DC), 4096 (member) | RAM in MB. |
| `cpus` | 2 | Virtual CPUs. |
| `ip` | DC `.10`, members `.20`, `.21`, ... in file order | Static IP: a full `a.b.c.d`, or just the last octet (`"55"` = `<subnet>.55`). |

A **user**:

| field | default | meaning |
|-------|---------|---------|
| `password` | (required) | Must meet Windows' complexity rules and must not contain the user name. |
| `groups` | `[]` | Groups besides Domain Users. A group that does not exist yet is created (a global security group). `"Domain Admins"` makes an administrator. |

The labs in `labs/`:

| lab | machines |
|-----|----------|
| `school` | DC1 (Server 2019) + CL1 (2012 R2, logs on as student1) + CL2 (2016, student2); users student1, student2, teacher |
| `2012`, `2016`, `2019` | a DC + one member of that version, logged on as `student`; `labadmin` is a Domain Admin |
| `win10`, `win11` | a 2019 DC + a Windows 10 / 11 member (win11 boots UEFI) |

Another lab: copy one, change the machine names, `network.subnet` and `network.lan_port`, and
`lab run` a config that names it.

## The run config

```json
{
  "lab": "school",
  "name": "school-smoke",
  "wait": 120,
  "timeout_min": 90,
  "mount": { "X": "../smoke-test/out" },
  "steps": [
    { "on": "all",          "as": "system",   "run": "X:/setup.bat" },
    { "on": "CL1",          "as": "student1", "run": "X:/smoke.exe" },
    { "on": ["CL1", "CL2"],                   "run": "X:/smoke.exe" },
    { "on": "DC1",          "as": "system",   "run": "X:/smoke.exe", "expect_exit": 0 }
  ]
}
```

| field | default | meaning |
|-------|---------|---------|
| `lab` | (required) | The lab to boot: `labs/<lab>.json`. |
| `name` | the file name without `.json` | A label for the run. |
| `wait` | 60 | Seconds a step may run before it counts as "still running" (see `keep_running`). `lab run --wait S` overrides it. |
| `timeout_min` | 60 | Give up on the whole run after this many minutes. `lab run --timeout-min M` overrides it. |
| `mount` | `{}` | Host folders as drives in the guests: `{ "X": "./app" }`. A relative path is relative to the run file. |
| `steps` | (at least one) | The programs to run, top to bottom. |

**Mounts.** Each folder becomes a read-only CD-ROM drive with exactly that letter, on every
machine that has a step on it. Letters are `A`-`Z`, but `C:` (system) and `D:` (the lab's
control disc) are reserved; at most 3 mounts. A mount is a **snapshot taken at launch**: the
guest cannot write to it, and host edits after the start are seen by the next run. (This is
how folders reach the guests without Samba or root on the host.)

A **step**:

| field | default | meaning |
|-------|---------|---------|
| `run` | (required) | The program: `X:/setup.bat`, `X:\app\run.exe`, `C:\Windows\System32\whoami.exe`. It must be on a mounted drive or `C:`, and end in `.exe`, `.bat`, `.cmd`, `.ps1` or `.msi` (`.msi` is installed with `msiexec /i ... /qn /norestart`). `/` and `\` both work. |
| `on` | `"all"` | Where: a machine name, a list of names, or `"all"` (every member, not the DC). |
| `as` | the machine's `logon` user | Who: `"system"` (the SYSTEM account: full rights, can install anything), or a user from the lab's `users`. The machine's own `logon` user runs in its logged-on desktop session; any other user is started with that user's password (no desktop). A step on the DC needs an explicit `as`. |
| `args` | none | Command-line arguments, as one string. |
| `expect_exit` | 0 | The exit code that counts as success. |
| `wait` | the run's `wait` | Seconds before this step counts as still running. |
| `keep_running` | `false` | If true, a program still running at `wait` counts as OK and is left running (servers, GUI apps). Otherwise it is stopped and the step fails with `timeout`. |

**Order.** Each machine runs its own steps in the order they appear in the file. Different
machines run at the same time, so a step on CL2 does not wait for an earlier step on CL1.
`lab run` exits 0 if every step returned its `expect_exit`, 1 if any did not.

## What a run produces

`labs/state/<lab>/runs/<YYYYmmdd-HHMMSS>/` (and `labs/state/<lab>/last` points at the latest):

    result.json          the summary: lab, config, run id, status ok|failed, and one record
                         per step: machine, user (system|logon|user), n, script, exit,
                         expect_exit, secs, ok
    config.json          a copy of the run config
    <machine>/step1out.txt, step1err.txt, ...   each step's stdout / stderr
    <machine>/screen.png                         the machine's screen at the end
    <machine>/serial.log                         the full guest event log

The VMs keep running after a run so you can look around; the VNC addresses are printed.
`lab restore <lab>` throws everything away again.

## Commands

    lab run CONFIG        restore the lab to its 0 state, boot it, run the config's steps
    lab up LAB            boot the lab from its 0 state and leave it running (no steps)
    lab status LAB        the 0 state, each machine (running / off, VNC), the last run
    lab restore LAB       stop the VMs and throw away everything since the 0 state (~1 s)
    lab stop LAB          power the VMs off, keeping their changes until the next run
    lab prepare LAB       build the 0 state now (the first run does it otherwise)
    lab rebuild LAB       delete the 0 state; the next run builds a new domain

`LAB` may also be a run config (it names its lab). Options: `--show` opens the VM windows
instead of VNC; `--updates` includes Windows Update when a base image has to be installed;
`--wait S`, `--timeout-min M`.

Exit codes: 0 all steps passed, 1 a step failed, 2 lab error, 3 timeout.
