# Environments: many copies of one lab, on the fly

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
domain, IPs and CA. What tells the copies apart is their name and id, which `wtl` and every
machine in them know. If the computers need other names too, `--rename` gives them real ones
([below](#new-computer-names---rename)).

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
- **Copies of a copy.** `wtl up school-exp42 --new` copies an environment (with its names, see
  below) in seconds, one level deep.

## New computer names: `--rename`

When the software under test reports its own computer name (to a server, in its logs), the
copies need names of their own. `--rename` renames machines for real: `hostname`,
`%COMPUTERNAME%` and every Windows API answer with the new name.

```
wtl up school --new --name exp42 --rename all      # DC1-EXP42, CL1-EXP42, CL2-EXP42
wtl up school --new --rename CL1                    # only CL1: CL1-K7Q3 (the random suffix)
wtl up single-2019 --new --name b7 --rename WS2019=BUILD7
wtl run tests.json --new --rename CL1 --rm         # throwaway: renamed copy, run, results kept, deleted
```

- **What to rename:** `KEY=NAME` (that name), `KEY` (the key plus the environment's suffix:
  `CL1-EXP42`), or `all`; repeat `--rename` or use commas. A machine you do not name keeps its
  name. Names are checked before anything boots: 1-15 of `A-Z 0-9 -`, unique, never cut short
  (a generated one that is too long is an error: name it, or use a shorter `--name`).
- **It takes minutes, once.** The renamed copy boots once, renames its machines and is saved; every
  run and `restore` after that starts renamed, as fast as any. A workgroup machine or a member
  reboots once, the DC twice (`netdom`: the new name is added, made primary, the old one
  removed; DHCP is authorized again under it).
- **A CA host you rename gets its CA reinstalled** under the new name (Windows cannot rename a
  CA's host): the same CA name and templates, and the members' certificates are requested again
  from it. Leave the CA host out of `--rename` and it keeps its name and its CA, which is faster.
  A renamed member gets a new machine certificate under its new name either way, and the DC
  deletes the old names from DNS.
- **Run files stay the same**: `"on": "CL1"` is always the lab's key. `${WTL_HOST_CA1}` in a
  run file's `run`, `args` or `mount` is the machine's computer name, and every step gets
  `WTL_HOSTNAME` (its own) and `WTL_HOST_<KEY>` (every machine's), so a test reaches
  `http://${WTL_HOST_CA1}/` whatever the names are.
- **`--rm`** (with `wtl run --new`) deletes the copy after the run, also when a step failed; its
  results move into the lab's `runs/` first, as `<time>-<environment>`.
- A lab file can also give a machine another computer name from the start (`"hostname"`, see
  [CONFIG.md](CONFIG.md)): a full build under that name.

`wtl status` shows each machine's computer name next to its key, and `wtl rebuild` of a renamed
environment renames it again from the lab's current 0 state.

## Where am I? Inside Windows

Every step of a `wtl run` can tell which lab, environment, machine and run it is in, so your
program or test knows where it is running. `wtl` sets it in every step's environment:
`WTL_ENVIRONMENT` (the environment's name; the lab's own name for the lab itself),
`WTL_ENVIRONMENT_ID`, `WTL_MACHINE` (the machine's key in the lab file), `WTL_HOSTNAME` (its
computer name), `WTL_HOST_<KEY>` (every machine's) and `WTL_RUN_ID` (`<time>-<id>`):

```bat
echo I am %WTL_MACHINE% (%COMPUTERNAME%) in %WTL_ENVIRONMENT% (%WTL_ENVIRONMENT_ID%)
```

On the host, `result.json` carries the same `environment`, `id` and `run_id`.

[examples/envs](https://github.com/mxrguspxrt/win-test-lab/tree/main/examples/envs) walks
through all of it, one command at a time, with `whereami.exe`: a small program that checks
inside each machine that the computer name and the step variables say what you expect, and
`run-all.sh`, which runs the whole walkthrough with every result checked (on one machine,
Server 2003, or an AD lab running next to its own copy), with or without `--rename`.
