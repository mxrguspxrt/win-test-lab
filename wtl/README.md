# wtl

The `wtl` command of [win-test-lab](https://crates.io/crates/win-test-lab): automated Windows
installs and tests, on many Windows versions, from a JSON file. Unattended installs from
Microsoft's ISOs, Active Directory labs, a frozen "0 state" every run starts from, on Linux,
macOS and Windows hosts with QEMU.

```
cargo install wtl
wtl doctor
```

This crate is only the command; the code is in
[win-test-lab](https://crates.io/crates/win-test-lab), and `cargo install win-test-lab` gives
the same `wtl`. Documentation, labs and examples:
https://github.com/mxrguspxrt/win-test-lab

License: MIT.
