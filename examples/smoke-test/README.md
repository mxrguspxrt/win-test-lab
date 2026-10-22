# smoke-test: a tiny standalone exe for `lab run`

    examples/smoke-test/build.sh                   # -> out/ (smoke.exe + setup.bat) and smoke.zip
    lab run examples/configs/2019-smoke.conf       # mount out/ as X:, install as admin, run as the student

`smoke.exe` is a single self-contained program - no DLLs, no data files. It prints who it runs
as (e.g. `LAB\student`) and the Windows version, then exits 0. A run that reaches `RESULT OK` /
exit 0 proves the lab booted and launched the exe.

The AD lab run config mounts `out/` as drive `X:`, runs `setup.bat` as admin (copies smoke.exe
to `C:\App`), then runs `smoke.exe` as `LAB\student` - see `examples/configs/2019-smoke.conf`.

`build.sh` also produces `smoke.zip` for the separate single-VM tester:
`launch-test.sh examples/smoke-test/smoke.zip smoke.exe 2012`.

Needs the mingw cross-compiler: `sudo apt-get install gcc-mingw-w64-x86-64`.
