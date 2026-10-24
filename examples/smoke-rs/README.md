# smoke-rs: one test program, two CPUs

`smoke.exe` in Rust, with no C runtime, built for **x64** and **Arm64** Windows from any host
that has [rustup](https://rustup.rs). No Visual Studio, Windows SDK or MinGW is needed: Rust's
own `rust-lld` links it, and `kernel32` is imported with `raw-dylib`.

    examples/smoke-rs/build.sh      # -> out/smoke-x64.exe, out/smoke-arm64.exe, out/setup.bat

Each exe prints who runs it, the CPU it was built for and the machine's CPU, then
`RESULT OK`, and exits 0:

    smoke.exe running as WSARM64\tester
    exe built for x64, machine CPU arm64 (running translated)
    RESULT OK

On Windows 11 on Arm the x64 exe runs too: Windows translates x64 code on the fly (Prism),
per program, with no setting. `examples/configs/single-win11-arm64-smoke.json` runs both.
