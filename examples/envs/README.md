# envs: environments, and a program that knows where it runs

`whereami.exe` runs inside a lab machine and checks that it knows which lab, environment and
machine it is: the computer name, `C:\WTL\instance.json`, the registry
(`HKLM\SOFTWARE\WinTestLab`, both views on 64-bit Windows) and the step's `WTL_*` variables must
all agree, and match what you expect. It ends with `RESULT OK` (exit 0) or `RESULT FAIL` (exit
1), so a `wtl run` passes only if every machine knows where it is.

Below, line by line, with what each command should show. `run-all.sh` does all of it with every
result checked, and stops at the first one that is wrong:

    examples/envs/build.sh
    examples/envs/run-all.sh single-2019      # single-2003, single-2022, ... or 2019 (an AD lab)
    examples/envs/run-all.sh 2019 --rename    # the same with every machine renamed (<KEY>-DEMO)

Running a lab also restores it, so do not point these at a lab you are using.

## 1. Build the program (once)

    examples/envs/build.sh

Needs `gcc-mingw-w64-x86-64` (and `gcc-mingw-w64-i686` for the 32-bit Server 2003). Writes
`out/whereami.exe` and `out32/whereami.exe`; the run files mount that folder as `X:`.

## 2. A new environment

    wtl up single-2019 --new --name demo --note "whereami demo"

`environment single-2019-demo (id 1a2b3c4d) made from lab single-2019's 0 state`, the machine
booting, then `environment single-2019-demo (id 1a2b3c4d) of lab single-2019` with its VNC
address. The first time, `wtl` builds the lab's 0 state first.

## 3. See it

    wtl list

A line `single-2019-demo   single-2019   1a2b3c4d   running   whereami demo`, and `single-2019`
itself with an id of its own.

    wtl status single-2019-demo
    wtl status single-2019-demo/WS2019

`environment single-2019-demo (id 1a2b3c4d) of lab single-2019`, the note, and its machine (the
second one: only `WS2019`).

## 4. Ask the machine where it is

    WTL_EXPECT_ENV=single-2019-demo WTL_EXPECT_ID=1a2b3c4d wtl run examples/envs/whereami.json --lab single-2019-demo

(`1a2b3c4d`: the id `wtl list` shows.) `ALL STEPS OK`. Each step's output (the run prints where
the results are, `<machine>/step1out.txt`) looks like:

    computer name   WS2019
    instance.json   lab=single-2019 environment=single-2019-demo id=1a2b3c4d machine=WS2019 run_id=20261124-101500-1a2b3c4d
                    note: whereami demo
    registry 64-bit  environment=single-2019-demo id=1a2b3c4d machine=WS2019 run_id=20261124-101500-1a2b3c4d
    registry 32-bit  environment=single-2019-demo id=1a2b3c4d machine=WS2019 run_id=20261124-101500-1a2b3c4d
    step variables  WTL_ENVIRONMENT=single-2019-demo WTL_ENVIRONMENT_ID=1a2b3c4d WTL_MACHINE=WS2019 WTL_RUN_ID=20261124-101500-1a2b3c4d
    RESULT OK: this is WS2019 of single-2019-demo (id 1a2b3c4d)

Step 1 runs as the logged-on user and checks the step variables too; step 2 runs as SYSTEM.
(A SYSTEM step sees the variables only on a 0 state built by `wtl` 0.3 or newer: there,
`WTL_SYSTEM_VARS=--vars` checks them as well.)

The lab itself, at the same time if you like (its id from `wtl list`): the computer is still
`WS2019`, but `environment=single-2019`:

    WTL_EXPECT_ENV=single-2019 WTL_EXPECT_ID=<its id> wtl run examples/envs/whereami.json --lab single-2019

A wrong expectation fails the run, which is the point: `WTL_EXPECT_ENV=single-2019 ... --lab
single-2019-demo` ends with `PROBLEM: the environment is "single-2019-demo", expected
"single-2019"`, `RESULT FAIL` and `A STEP FAILED`.

## Renamed copies

The same with real new computer names:

    wtl up single-2019 --new --name ren --rename all

`=== renaming the machines of environment single-2019-ren: WS2019 -> WS2019-REN ===`, a reboot,
`WS2019: WTL LAB_RENAMED machine=WS2019 from=WS2019 to=WS2019-REN`, and `machines renamed and
saved`. `wtl status single-2019-ren/WS2019` shows `hostname WS2019-REN`. Then:

    WTL_EXPECT_ENV=single-2019-ren WTL_EXPECT_ID=<its id> WTL_EXPECT_HOST=WS2019-REN wtl run examples/envs/whereami.json --lab single-2019-ren

prints `computer name   WS2019-REN`, `hostname=WS2019-REN` in `instance.json` and the registry,
`WTL_HOSTNAME=WS2019-REN`, and `RESULT OK: this is WS2019 (WS2019-REN) of single-2019-ren`. The
lab itself, at the same time, still says `WS2019`.

## 5. Fresh again, then gone

    wtl rebuild single-2019-demo      # fresh again from single-2019's 0 state, same name and id
    wtl rm single-2019-demo           # its VMs, disks and results
    wtl list                          # no single-2019-demo

## Other labs

- **Server 2003** (32-bit, no PowerShell): `examples/envs/whereami-2003.json` (mounts `out32/`),
  `run-all.sh single-2003`.
- **An AD lab**: `examples/envs/whereami-ad.json` checks the member `CL2019` and the DC `DC2019`
  of the lab `2019`; `run-all.sh 2019` runs the lab and its environment at the same time, so two
  copies of the same domain (same DC, IPs and names) are up at once on separate networks.
- Any other single-machine lab: `whereami.json` with `--lab`, e.g. `run-all.sh single-2022`.
