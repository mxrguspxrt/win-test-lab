# prepare.ps1 - the lab's first setup script (once, as SYSTEM, while its 0 state is built):
#  - turn on the Windows Hypervisor Platform, the API behind QEMU's whpx accelerator. Windows
#    then starts on Hyper-V, which needs the CPU's virtualization (AMD-V / VT-x) inside this VM:
#    on a Linux host, KVM passes it on when its "nested" option is on. It takes effect after a
#    reboot, and the build reboots after its setup scripts.
#  - let Defender skip QEMU's folder and installer: scanning its ~1.2 GB of DLLs makes the
#    silent install (the next setup item) take over 10 minutes instead of about 3.
$ErrorActionPreference = 'Stop'
$f = Enable-WindowsOptionalFeature -Online -FeatureName HypervisorPlatform -All -NoRestart
"HypervisorPlatform enabled (restart needed: $($f.RestartNeeded))"
Add-MpPreference -ExclusionPath (Join-Path $env:ProgramFiles 'qemu')
Add-MpPreference -ExclusionProcess 'qemu-w64-setup.exe'
"Defender skips $env:ProgramFiles\qemu and qemu-w64-setup.exe"
exit 0
