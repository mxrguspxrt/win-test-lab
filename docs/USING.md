# Use win-test-lab from your own project

Keep a small `wintest/` folder in your project, describing which Windows machines you need and
what to run on them, and test every build on real Windows with one command:

    lab run wintest/run-2019.json

## Install once

On a Linux machine with KVM (a desktop, a server, or a self-hosted CI runner):

    git clone https://github.com/mxrguspxrt/win-test-lab.git ~/win-test-lab
    cd ~/win-test-lab
    ./setup-host.sh                                   # QEMU, OVMF, /dev/kvm access
    cd wtl-rs && cargo build --release && cd ..       # needs Rust: https://rustup.rs
    ln -sfn "$PWD/wtl-rs/target/release/wtl-rs" ~/.local/bin/lab

Then put the Windows evaluation ISOs you need into `~/win-test-lab/iso-files/` (see
[ISOS.md](ISOS.md); `./download-isos.sh 2019` tries to fetch one). `lab` finds win-test-lab
from its own location, wherever you call it from.

**The first run of a lab is slow, later runs are fast:**

| what | when | time |
|------|------|------|
| install Windows into a base image (one per OS and role: `2019-client`, `2012-dc`, ...) | once per machine, the first time a lab needs it | 10-30 min each, in parallel (hours with `--updates`) |
| build the lab's 0 state (rename, domain, join, users, autologon) | once per lab, and after `lab rebuild` | 3-5 min (workgroup) to ~20 min (AD) |
| a run: boot from the 0 state, run your steps | every time | 1-3 min + your steps |

## A project layout

    myapp/
      wintest/
        lab.json            # optional: the machines this project needs (or use a lab from win-test-lab)
        run-2019.json       # one run file per thing to test
        install.ps1         # your own scripts, run inside Windows
        smoke.ps1
      build/out/            # your build output, mounted into Windows as a drive

A run file names its lab and mounts folders as drives. Paths are relative to the run file:

```json
{
  "lab": "./lab.json",
  "mount": { "X": "../build/out", "Y": "." },
  "steps": [
    { "as": "system", "run": "Y:/install.ps1", "args": "-Source X:\\myapp.msi" },
    { "run": "Y:/smoke.ps1" }
  ]
}
```

`"lab"` is either a path to a lab file (`"./lab.json"`, `"../labs/ad.json"`) or the name of a
lab that ships with win-test-lab (`"single-2019"`, `"mixed"`, `"school"`; see
`~/win-test-lab/labs/`). Each lab file gets its own 0 state: a project lab is stored as
`~/win-test-lab/labs/state/<file name>-<hash of its path>/` (set `"name": "myapp"` in the lab
file for a nicer folder name; then keep it unique across your projects).

A step's exit code decides pass/fail: write your scripts so they exit non-zero on failure (in
PowerShell: `exit 1`, or `$ErrorActionPreference = 'Stop'` and let it throw). Each step's
stdout/stderr is collected.

## One machine (workgroup mode)

No domain, one Windows machine, logged on as a local user. This is the right default for
"does my program install and start on Windows Server 2019?":

```json
{
  "machines": {
    "WS2019": { "os": "2019", "logon": "tester" }
  },
  "users": {
    "tester": { "password": "Test!2026" },
    "admin2": { "password": "Second!2026", "groups": ["Administrators"] }
  }
}
```

- No `domain`, no `role: "dc"`: the lab is a workgroup. Users are local accounts; `groups` are
  local groups (`Administrators`, `Remote Desktop Users`, or new ones, which are created).
- One machine needs no `network`; it has internet through the host (NAT). For several
  workgroup machines that talk to each other, add `"network": { "subnet": "10.77.50",
  "lan_port": 47050 }` (they get .20, .21, ...).
- Steps: `"as": "system"` (full rights; installers), the default (the logged-on user, with a
  desktop), or `"as": "admin2"` (another local user, started with its password, no desktop).

win-test-lab ships `labs/single-2012.json`, `single-2016`, `single-2019`, `single-win10` and
`single-win11`, so a project can also just say `"lab": "single-2019"`.

## A domain (AD mode)

Give one Windows Server `"role": "dc"` and the lab becomes an Active Directory domain. Any
Server version can be the DC, and any OS a member. The user's example: a Server 2012 R2 domain
controller with a Server 2019 member server:

```json
{
  "domain": "lab.local",
  "netbios": "LAB",
  "network": { "subnet": "10.77.40", "lan_port": 47040 },
  "machines": {
    "DC1":  { "os": "2012", "role": "dc" },
    "SRV1": { "os": "2019", "logon": "operator" }
  },
  "users": {
    "operator": { "password": "Oper!2026", "groups": ["Domain Admins"] },
    "appuser":  { "password": "Runs!2026app" }
  }
}
```

(that is `labs/mixed.json`). `SRV1` joins `lab.local` and logs on as `LAB\operator`; users are
domain users; `"on": "DC1"` runs a step on the domain controller (with an explicit `as`). Add
more members (clients `win10`/`win11`, more servers) as more machines.

```json
{
  "lab": "mixed",
  "mount": { "X": "../build/out" },
  "steps": [
    { "on": "SRV1", "as": "system",  "run": "X:/install.bat" },
    { "on": "SRV1",                  "run": "X:/smoke.exe" },
    { "on": "SRV1", "as": "appuser", "run": "X:/smoke.exe" },
    { "on": "DC1",  "as": "system",  "run": "X:/check-ad.ps1" }
  ]
}
```

Every field of both files: [CONFIG.md](CONFIG.md).

## Roles and your own setup

A machine can carry server roles baked into the 0 state, extra Windows features, and your own
setup scripts — so a run starts against a domain that already has a CA, a DHCP scope, a web
server, and whatever your scripts arranged. This is for testing against realistic infra, not
just a bare domain.

```json
{
  "domain": "pki.local", "netbios": "PKI",
  "network": { "subnet": "10.77.60", "lan_port": 47060 },
  "machines": {
    "DC1": { "os": "2019", "roles": ["ad-ds", "dhcp"], "dhcp": { "range": ".100-.199" } },
    "CA1": { "os": "2012", "roles": ["ad-cs", "iis"], "ad_cs": { "ca_name": "PKI-ROOT-CA" },
             "features": ["Telnet-Client"],
             "setup": [ { "run": "./setup/configure-ca.ps1" } ] },
    "CL1": { "os": "2016", "logon": "student" }
  },
  "users": { "student": { "password": "Pupil!2026", "groups": ["Domain Users"] } }
}
```

- `roles`: `ad-ds` (the DC), `ad-cs` (an Enterprise Root CA with auto-enrollment), `dhcp`,
  `iis`. `role: "dc"` is still the short alias for `roles: ["ad-ds"]`.
- `features`: any `Install-WindowsFeature` name; an unknown one fails the build with the list.
- `setup`: your scripts (relative to the lab file), run once during the build after the join,
  roles and features, as `system` or a lab user.

Everything here is applied while the 0 state is built and frozen into it, so changing roles,
features or a setup script means `lab rebuild <lab>` before the next run. `labs/pki.json` and
`examples/configs/pki-smoke.json` in win-test-lab are a worked example (a CA, DHCP, IIS, and
checks for the issued certificates, the web site and the scope). Full reference:
[CONFIG.md](CONFIG.md).

## Several Windows versions

One run file per version (they can share the scripts), and a loop:

```bash
#!/usr/bin/env bash
# wintest/all.sh - run the smoke test on every Windows version, report each
set -u
cd "$(dirname "$0")"
rc=0
for v in 2012 2016 2019 win10 win11; do
    echo "=== $v"
    lab run "run-$v.json" || rc=1
    lab restore "single-$v" >/dev/null
done
exit "$rc"
```

with e.g. `run-2016.json` = `{ "lab": "single-2016", "mount": {...}, "steps": [...] }`. The labs
run one after the other; on a bigger host two labs may run at once if their `lan_port`s and
VNC displays differ. (A `targets` matrix inside one run file does not exist yet.)

## Exit codes and results

| exit | meaning |
|------|---------|
| 0 | every step exited with its `expect_exit` (default 0) |
| 1 | at least one step did not |
| 2 | lab error: bad config, a machine failed to come up, a VM crashed |
| 3 | timeout (`timeout_min`, or `--timeout-min`) |

Results: `~/win-test-lab/labs/state/<lab>/runs/<time>/` (and `.../last` points at the newest):
`result.json` (every step with machine, user, exit code, duration, ok), `config.json`, and per
machine `step<N>out.txt` / `step<N>err.txt`, `screen.png`, `serial.log`. The run prints this
path at the end. The VMs stay up for a look over VNC; `lab restore <lab>` stops them.

## CI

Any CI system works if the runner is a Linux machine with KVM (cloud runners without nested
virtualization do not). A self-hosted GitHub Actions runner with win-test-lab installed as
above:

```yaml
jobs:
  windows-smoke:
    runs-on: [self-hosted, linux, kvm]
    timeout-minutes: 90
    steps:
      - uses: actions/checkout@v4
      - run: ./build.sh                          # produces build/out/
      - run: lab run wintest/run-2019.json
      - if: always()
        run: |
          cp -rL ~/win-test-lab/labs/state/single-2019/last wintest-results
          lab restore single-2019
      - if: always()
        uses: actions/upload-artifact@v4
        with: { name: wintest-results, path: wintest-results }
```

Build the base images and 0 states once on the runner beforehand (`lab prepare single-2019`),
so CI jobs only pay for the run itself. Runs of the same lab must not overlap: give the job a
`concurrency:` group per lab.

## Limits

- **ISOs are not redistributable**: every machine that runs labs downloads its own evaluation
  ISOs and builds its own base images; images and 0 states are never shared or checked in.
- **Evaluation licenses expire**: Windows Server evaluations run 180 days, Windows 10/11
  Enterprise evaluations 90 days, counted from the base image's install. Rebuild the base
  images (delete `images/<os>-*.qcow2`) before they run out.
- **Disk**: about 10 GB per base image, a few GB per lab 0 state, and up to the guest's
  growth per run (thrown away by `lab restore`); 60 GB free is a comfortable minimum.
- **RAM and CPU**: 4 GB and 2 vCPUs per machine by default (3 GB for a DC), so a 3-machine lab
  wants 12 GB free; set `mem` / `cpus` per machine.
- Mounted folders are read-only snapshots taken at the start of a run; use the step's output
  (stdout/stderr) to get data out.
- Linux + KVM is the tested host. Windows (WHPX) and Intel macOS (HVF) builds compile but are
  not tested.
