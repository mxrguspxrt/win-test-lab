# smoke-test: a tiny standalone exe for `wtl run`

    examples/smoke-test/build.sh                    # -> out/ (smoke.exe + setup.bat) and smoke.zip
    wtl run examples/configs/school-smoke.json      # mount out/ as X:, run it on every machine

`smoke.exe` is a single self-contained program - no DLLs, no data files. It prints who it runs
as (e.g. `LAB\student1`) and the Windows version, then exits 0. A run whose steps all exit 0
proves the lab booted, the mount worked and the program started as the right user.

The example configs mount `out/` as drive `X:`, run `setup.bat` as `system` (copies smoke.exe
to `C:\App`), then `smoke.exe` as the machines' logged-on users - see
`examples/configs/school-smoke.json` and `2019-smoke.json`.

Needs the mingw cross-compiler: `sudo apt-get install gcc-mingw-w64-x86-64`.
