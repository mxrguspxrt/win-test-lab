# nested: virtual machines inside a lab machine

A lab machine is a VM, and with nested virtualization it can run VMs of its own. This example
builds a Windows 11 lab with the Windows Hypervisor Platform turned on and QEMU for Windows
installed, and its run boots a tiny VM inside it twice: on the hardware (`-accel whpx`) and on
QEMU's software CPU (`-accel tcg`).

    examples/nested/build.sh               # once: the tiny VM (needs nasm) and QEMU for Windows (197 MB)
    wtl run examples/nested/nested.json    # the first time it also builds the lab (~10 minutes)

The host has to pass its CPU's virtualization on: `wtl` starts Windows with `-cpu host`, and on
Linux KVM's `nested` option must be on (`cat /sys/module/kvm_amd/parameters/nested`, or
`kvm_intel`, says `1` or `Y`; the default since Linux 4.20). Tested on Linux with KVM on AMD.

## What is here

- `lab.json`: the lab `win11-nested`, one Windows 11 machine (`WSWIN11`, logged on as `tester`),
  with two setup items run once while its 0 state is built: `prepare.ps1` (the Hypervisor
  Platform on, Defender told to skip QEMU) and QEMU for Windows' silent installer. The build
  reboots after them, so Windows starts on Hyper-V from then on.
- `hello.asm`: the tiny VM, one 512-byte boot sector. It says on its serial port which hypervisor
  it runs on, counts to 2^30 and times that with the BIOS clock, and ends QEMU with exit code 33.
- `run-vm.ps1`: the step: runs QEMU on `out\hello.img` with one accelerator and checks what the
  VM said (`RESULT OK`, exit 0).
- `nested.json`: the run file: this folder as `X:`, the two steps as the logged-on user (a plain
  user: QEMU with whpx needs no administrator).

## What a run shows

`ALL STEPS OK`, and in each step's output (`<results>/WSWIN11/step1out.txt`, `step2out.txt`):

    windows         Microsoft Windows 11 Pro on AMD Ryzen 3 PRO 7335U with Radeon Graphics
    hypervisor      under this Windows: True
    qemu            QEMU emulator version 11.1.0 (v11.1.0-12130-ge470268ff4) (43.5 s to start)
    qemu            C:\Program Files\qemu\qemu-system-x86_64.exe -accel whpx,kernel-irqchip=off -cpu qemu64,-svm -machine pc -m 16 -nodefaults -display none -no-reboot -drive file=X:\out\hello.img,format=raw,if=ide,snapshot=on -serial file:C:\Users\tester\AppData\Local\Temp\nested-whpx\serial.txt -device isa-debug-exit,iobase=0xf4,iosize=0x04
    vm says         NESTED hello from a VM inside Windows
    vm says         NESTED hypervisor: Microsoft Hv
    vm says         NESTED counted to 2^30 in 18 BIOS ticks (55 ms each)
    qemu exit       33 after 2 s (whpx)
    vm's own loop   1 s of that (the rest: QEMU and the VM's BIOS starting)
    RESULT OK: a VM ran inside this Windows (whpx, 2 s)

    windows         Microsoft Windows 11 Pro on AMD Ryzen 3 PRO 7335U with Radeon Graphics
    hypervisor      under this Windows: True
    qemu            QEMU emulator version 11.1.0 (v11.1.0-12130-ge470268ff4) (0.3 s to start)
    qemu            C:\Program Files\qemu\qemu-system-x86_64.exe -accel tcg -machine pc -m 16 -nodefaults -display none -no-reboot -drive file=X:\out\hello.img,format=raw,if=ide,snapshot=on -serial file:C:\Users\tester\AppData\Local\Temp\nested-tcg\serial.txt -device isa-debug-exit,iobase=0xf4,iosize=0x04
    vm says         NESTED hello from a VM inside Windows
    vm says         NESTED hypervisor: TCGTCGTCGTCG
    vm says         NESTED counted to 2^30 in 111 BIOS ticks (55 ms each)
    qemu exit       33 after 6.6 s (tcg)
    vm's own loop   6.1 s of that (the rest: QEMU and the VM's BIOS starting)
    RESULT OK: a VM ran inside this Windows (tcg, 6.6 s)

`Microsoft Hv` is the hypervisor the tiny VM sees under whpx: Windows' Hyper-V, itself on the
host's KVM, so three hypervisors deep, and still at the hardware's speed: it counts to 2^30 in
0.5-1 s, where tcg (QEMU translating every instruction) needs 6-8 s, and the whole VM boots and
exits in 1-2 s. QEMU's own first start after Windows boots takes 15-45 s (Windows loads and
checks its ~100 DLLs), so `run-vm.ps1` starts it once (`--version`) before it times the VM.

The plain `single-win11` lab has no Hypervisor Platform: there `-accel tcg` works too (with QEMU
installed in a step), and whpx fails with `WHPX: No accelerator found`.
