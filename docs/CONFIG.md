# Labs and run configs

Two small JSON files describe everything:

- a **lab** (`labs/<name>.json` in win-test-lab, or a `lab.json` in your own project): the
  machines, the users, and either an AD domain or a workgroup. It is built once into a
  **0 state** (Windows installed, machines named, the domain created and joined or the local
  users made, everyone logged on) and then reused by every run.
- a **run config**, any `<something>.json`: which lab to boot, which host folders to mount
  as drives, and which programs to run on which machine as whom.

Using it from another project: [USING.md](USING.md).

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

## The lab file

A lab has one of two **modes**, decided by its machines:

- **AD mode**: one machine has `"role": "dc"`. It becomes the domain controller of a new
  forest (`domain`, `netbios` required); the other machines join the domain; `users` are
  domain users; machines log on as `LAB\<user>`. Any Windows Server version can be the DC,
  and any OS a member (a member server, or a Windows 10/11 client), in any mix.
- **Workgroup mode** (single mode): no machine has `role: "dc"`. There is no domain
  (`domain`/`netbios` must be absent); `users` are local accounts created on every machine,
  `groups` are local groups; each machine logs on as `<MACHINE>\<user>`. One machine needs no
  `network`.

An AD lab, `labs/school.json`:

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

A workgroup lab with one machine, `labs/single-2019.json`:

```json
{
  "machines": { "WS2019": { "os": "2019", "logon": "tester" } },
  "users":    { "tester": { "password": "Test!2026" } }
}
```

**The lab's name** names its state folder (`labs/state/<name>/`) and its VMs. A lab in
win-test-lab's `labs/` is called by its file name (`labs/school.json` is `school`). A lab file
anywhere else gets its file name plus a hash of its full path (`/home/me/myapp/wintest/lab.json`
becomes e.g. `lab-3f9a1c2b`), so two projects' `lab.json` never share a 0 state; set `"name"`
to choose it yourself (then keep it unique). `lab status <run or lab file>` shows it.

| field | required | meaning |
|-------|----------|---------|
| `name` | no | The state folder / VM name (see above). `A-Z 0-9 . _ -`. |
| `domain` | AD mode | DNS name of the new AD forest, e.g. `lab.local`. Must be absent in workgroup mode. |
| `netbios` | AD mode | NetBIOS domain name (1-15 of `A-Z 0-9 . _ -`); users log on as `LAB\student1`. Must be absent in workgroup mode. |
| `admin_password` | no (default `WinTest!2026`) | Password of the `Administrator`: the domain Administrator (also the DSRM password) in AD mode, every machine's local Administrator in workgroup mode. Must meet Windows' complexity rules and must not contain the account name. |
| `network.subnet` | AD mode, or several machines | First three octets of the lab's private /24, e.g. `10.77.30`. |
| `network.lan_port` | with `network` | UDP port of the lab's private LAN on the host (any free port; two labs that run at the same time need different ports). |
| `machines` | yes | The machines, keyed by **name**. The name is also the Windows computer name (1-15 of `A-Z 0-9 . _ -`). At most one has `role: "dc"`. |
| `users` | no | Domain users (AD) or local users on every machine (workgroup), keyed by user name. Default: none. |

Without `network` (a single workgroup machine) the machine has only the host's NAT network
(internet, no lab LAN). With it, every machine gets a second NIC on the private lab LAN.

A **machine**:

| field | default | meaning |
|-------|---------|---------|
| `os` | (required) | OS key from `lib/oslist.conf`: `2012`, `2016`, `2019`, `2022`, `2025`, `win10`, `win11` (aliases like `2012r2`, `ws2019` work too). A machine with the `ad-ds` role (the DC) must be a Windows Server. |
| `roles` | `[]` | The server roles baked into this machine's 0 state: any of `ad-ds`, `ad-cs`, `dhcp`, `iis` (see **Roles** below). Servers only. Mutually exclusive with `role`. |
| `role` | `"member"` | The compact alias for `roles`: `"dc"` means `roles: ["ad-ds"]` (the domain controller; at most one, and it makes the lab an AD lab); `"member"` means no roles. Mutually exclusive with `roles`. |
| `features` | `[]` | Windows feature names installed during the build with `Install-WindowsFeature`, e.g. `["Telnet-Client"]`. Servers only; an unknown name fails the build with the list of available features. |
| `setup` | `[]` | Scripts run once during the build, after the join / roles / features (see **Setup scripts** below). |
| `ad_cs`, `dhcp`, `iis` | (defaults) | Options for the matching role (see **Roles**). Ignored when the role is absent. |
| `logon` | Administrator | A user from `users` that this machine logs on as automatically; without one it logs on as Administrator (domain or local). The DC cannot set one. Steps run as this user by default. |
| `mem` | 3072 (DC), 4096 (others) | RAM in MB. |
| `cpus` | 2 | Virtual CPUs. |
| `ip` | DC `.10`, the others `.20`, `.21`, ... in file order | Static IP on the lab LAN: a full `a.b.c.d`, or just the last octet (`"55"` = `<subnet>.55`). Needs `network`. |

A machine's Windows install comes from a base image: `images/<os>-dc.qcow2` for the DC,
`images/<os>-client.qcow2` for every other machine (machines with the same OS share it).
Missing images are installed automatically by the first run.

### Roles

A machine lists the server roles it runs in `roles` (or, for the common domain controller,
the `role: "dc"` alias). Roles are installed and configured **during the 0-state build** and
frozen into it, so a running lab starts with the CA, DHCP scope, IIS site and so on already up.
Changing a machine's roles, features or setup scripts means rebuilding: `lab rebuild <lab>`,
then run again.

| role | what the build does | options |
|------|---------------------|---------|
| `ad-ds` | AD Domain Services: promotes this machine to the domain controller of a new forest. Makes the lab an AD lab (needs `domain`/`netbios`). One per lab. (`role: "dc"` is the alias.) | — |
| `ad-cs` | AD Certificate Services: an Enterprise Root CA. Needs an AD lab; the machine joins the domain, then installs the CA as a domain admin. With `autoenroll`, a domain GPO ("WTL Certificate Auto-Enrollment") turns on computer and user certificate auto-enrollment, and two templates are published: `Workstation` (computers; Domain Computers may auto-enroll) and `WTLUser`, a version 2 copy of the built-in User template (Windows never auto-enrolls the version 1 User template), with the UPN in the subject alternative name and auto-enroll for Domain Users. Members get a computer and a user certificate shortly after logon. | `ad_cs`: `ca_name` (default `<NETBIOS>-ROOT-CA`), `key_length` (2048), `validity_years` (5), `autoenroll` (true) |
| `dhcp` | DHCP server: a scope for the lab subnet (router and DNS = the DC), authorized in AD when there is a domain. | `dhcp`: `range`, the scope as last octets within the subnet, `".<start>-.<end>"` (default `.100-.199`) |
| `iis` | IIS web server: `Web-Server` + the management console (and ASP.NET 4.5 where available), the default site serving a test page reachable at `http://<machine>/` from other lab machines. | `iis`: `site_path` (default `C:\inetpub\wwwroot`) |

`ad-cs` needs an AD lab; a standalone (workgroup) CA is not supported yet. A standalone CA
would be a reasonable follow-up.

### Setup scripts

`setup` runs your own scripts once during the build, in order, after the machine has joined,
installed its roles and installed its features:

```json
"setup": [
  { "run": "./setup/harden.ps1" },
  { "run": "setup/seed-data.bat", "args": "--demo", "as": "operator" }
]
```

| field | default | meaning |
|-------|---------|---------|
| `run` | (required) | The script, **relative to the lab file**: `./setup/x.ps1`, `setup/x.bat`. `.exe .bat .cmd .ps1 .msi`. It is staged onto the control disc and run during the build (not from a host mount). |
| `args` | none | Command-line arguments, as one string. |
| `as` | `"system"` | Who runs it: `"system"` (full rights) or a lab user (a domain user in AD mode). |

A setup script that exits non-zero (or times out) fails the build with its output, like a
failed step. Because setup scripts run while the 0 state is being built, their effects are
baked in and every run starts with them already applied.

### Build order

The 0 state is built in stages so dependencies line up: the **DC** first (AD DS + forest +
users, then its own roles such as `dhcp`); then the **other machines** join and, if they have
any, run their `configure` stage (roles like `ad-cs`/`iis`, then features, then setup scripts)
as the Administrator before switching to their logon user; finally each machine logs on as its
final user and the 0 state is frozen. At **run** start, members in an AD lab run
`gpupdate /force`, `certutil -pulse` and `certutil -user -pulse` before the steps. Enrollment
still finishes in the background, so a step that needs a certificate should wait for it for a
minute or two (as `examples/pki/*-cert.ps1` do).

A **user**:

| field | default | meaning |
|-------|---------|---------|
| `password` | (required) | Must meet Windows' complexity rules (8+ characters from 3 of: upper, lower, digit, symbol) and must not contain the user name (checked before boot). |
| `groups` | `[]` | AD mode: domain groups besides Domain Users (`"Domain Admins"` makes an administrator). Workgroup mode: local groups besides Users (`"Administrators"`). A group that does not exist yet is created. |

The labs in `labs/`:

| lab | mode | machines |
|-----|------|----------|
| `single-2012`, `single-2016`, `single-2019`, `single-win10`, `single-win11` | workgroup | one machine of that version, logged on as the local user `tester` |
| `mixed` | AD | DC1 (Server 2012 R2) + SRV1 (Server 2019 member server, logs on as `LAB\operator`, a Domain Admin); also the user `appuser` |
| `school` | AD | DC1 (Server 2019) + CL1 (2012 R2, logs on as student1) + CL2 (2016, student2); users student1, student2, teacher |
| `2012`, `2016`, `2019` | AD | a DC + one member of that version, logged on as `student`; `labadmin` is a Domain Admin |
| `win10`, `win11` | AD | a 2019 DC + a Windows 10 / 11 member (win11 boots UEFI) |
| `pki` | AD | DC1 (2019, `ad-ds` + `dhcp`) + CA1 (2012 R2, `ad-cs` Enterprise Root CA with auto-enrollment + `iis`) + CL1 (2016, logs on as `student`). `examples/configs/pki-smoke.json` checks the certs, the web site and the DHCP scope. |

Another lab: copy one, change the machine names (and for a LAN `network.subnet` and
`network.lan_port`), and `lab run` a config that names it.

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
| `lab` | (required) | The lab to boot: a name (`"school"` = `labs/school.json` in win-test-lab) or a path to a lab file, relative to this run file (`"./lab.json"`, `"../labs/ad.json"`). Anything with a `/` or `\`, or ending in `.json`, is a path. |
| `name` | the file name without `.json` | A label for the run. |
| `wait` | 60 | Seconds a step may run before it counts as "still running" (see `keep_running`). `lab run --wait S` overrides it. |
| `timeout_min` | 60 | Give up on the whole run after this many minutes. `lab run --timeout-min M` overrides it. |
| `mount` | `{}` | Host folders as drives in the guests: `{ "X": "./app" }`. A relative path is relative to the run file. |
| `steps` | (at least one) | The programs to run, top to bottom. |

**Mounts.** Each folder becomes a read-only CD-ROM drive with exactly that letter, on every
machine that has a step (all the run's drives, so a script on `Y:` can install `X:\app.msi`).
Letters are `A`-`Z`, but `C:` (system) and `D:` (the lab's
control disc) are reserved; at most 3 mounts. A mount is a **snapshot taken at launch**: the
guest cannot write to it, and host edits after the start are seen by the next run. (This is
how folders reach the guests without Samba or root on the host.)

A **step**:

| field | default | meaning |
|-------|---------|---------|
| `run` | (required) | The program: `X:/setup.bat`, `X:\app\run.exe`, `C:\Windows\System32\whoami.exe`. It must be on a mounted drive or `C:`, and end in `.exe`, `.bat`, `.cmd`, `.ps1` or `.msi` (`.msi` is installed with `msiexec /i ... /qn /norestart`). `/` and `\` both work. |
| `on` | `"all"` | Where: a machine name, a list of names, or `"all"` (every machine except the DC). |
| `as` | the logged-on user | Who: `"system"` (the SYSTEM account: full rights, can install anything), or a user from the lab's `users` (a domain user in AD mode, a local user in workgroup mode). The machine's logged-on user (its `logon` user, else Administrator) runs in its desktop session; any other user is started with that user's password (no desktop). A step on the DC needs an explicit `as`. |
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
