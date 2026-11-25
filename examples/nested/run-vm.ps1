# run-vm.ps1 - a run step inside Windows: boot out\hello.img (next to this script) in the QEMU
# for Windows the lab installed, with one accelerator, and check what the tiny VM says.
#
#   -Accel whpx   hardware virtualization through the Windows Hypervisor Platform: inside a lab
#                 machine that is nested virtualization (the host's AMD-V / VT-x, passed on by KVM)
#   -Accel tcg    QEMU's own software CPU: works anywhere, slower
#   (-Accel whpx,kernel-irqchip=on and the like pass QEMU's own accelerator options)
#
# Prints the VM's serial output and how long it took; ends with RESULT OK (exit 0) or
# RESULT FAIL (exit 1).
param([string]$Accel = 'whpx')
$ErrorActionPreference = 'Stop'

$cs = Get-CimInstance Win32_ComputerSystem
$cpu = Get-CimInstance Win32_Processor | Select-Object -First 1
"windows         $((Get-CimInstance Win32_OperatingSystem).Caption) on $($cpu.Name.Trim())"
"hypervisor      under this Windows: $($cs.HypervisorPresent)"

$qemu = Join-Path $env:ProgramFiles 'qemu\qemu-system-x86_64.exe'
if (-not (Test-Path $qemu)) { "PROBLEM: no $qemu (the lab's setup installs QEMU for Windows)"; "RESULT FAIL"; exit 1 }
$img = Join-Path $PSScriptRoot 'out\hello.img'
if (-not (Test-Path $img)) { "PROBLEM: no $img (run examples/nested/build.sh)"; "RESULT FAIL"; exit 1 }
# QEMU's first start after Windows boots takes 15-45 s (Windows loads and checks its ~100
# DLLs): do it once here, so the time below is just the VM's
$sw = [System.Diagnostics.Stopwatch]::StartNew()
$ver = (& $qemu --version | Select-Object -First 1)
"qemu            $ver ($([Math]::Round($sw.Elapsed.TotalSeconds, 1)) s to start)"

$dir = Join-Path $env:TEMP ("nested-" + ($Accel -replace '[^A-Za-z0-9]', '-'))
New-Item -ItemType Directory -Force -Path $dir | Out-Null
$serial = Join-Path $dir 'serial.txt'
Remove-Item $serial -ErrorAction SilentlyContinue

# the disk is on a read-only CD: snapshot=on keeps the VM's writes in a throwaway overlay;
# isa-debug-exit lets the VM end QEMU with an exit code of its own (33 = it got to the end).
# whpx: the default CPU model asks for AMD-V (svm), which Hyper-V does not pass on to a VM
$a = @('-accel', $Accel)
if ($Accel -eq 'whpx') { $a = @('-accel', 'whpx,kernel-irqchip=off') }
if ($Accel -like 'whpx*') { $a += @('-cpu', 'qemu64,-svm') }
$qargs = $a + @('-machine', 'pc', '-m', '16', '-nodefaults', '-display', 'none', '-no-reboot',
    '-drive', "file=$img,format=raw,if=ide,snapshot=on", '-serial', "file:$serial",
    '-device', 'isa-debug-exit,iobase=0xf4,iosize=0x04')
"qemu            $qemu $($qargs -join ' ')"

$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = $qemu
$psi.Arguments = $qargs -join ' '
$psi.UseShellExecute = $false
$psi.RedirectStandardOutput = $true
$psi.RedirectStandardError = $true
$sw = [System.Diagnostics.Stopwatch]::StartNew()
$p = [System.Diagnostics.Process]::Start($psi)
$out = $p.StandardOutput.ReadToEndAsync()
$err = $p.StandardError.ReadToEndAsync()
$timedOut = -not $p.WaitForExit(240000)
if ($timedOut) { $p.Kill() }
$p.WaitForExit()
$secs = [Math]::Round($sw.Elapsed.TotalSeconds, 1)
$code = $p.ExitCode

foreach ($l in ($out.Result + $err.Result) -split "`r?`n") { if ($l.Trim()) { "qemu says       $l" } }
$said = @()
if (Test-Path $serial) { $said = @(Get-Content $serial) }
foreach ($l in $said) { "vm says         $l" }
"qemu exit       $code after $secs s ($Accel)"
$loop = $said | Select-String -Pattern 'in (\d+) BIOS ticks' | Select-Object -First 1
if ($loop) { "vm's own loop   $([Math]::Round([int]$loop.Matches[0].Groups[1].Value / 18.2, 1)) s of that (the rest: QEMU and the VM's BIOS starting)" }

$problems = @()
if ($timedOut) { $problems += 'QEMU did not finish in 240 s' }
elseif ($code -ne 33) { $problems += "QEMU exited $code, not 33 (the VM did not get to its end)" }
if (-not ($said -match '^NESTED hello')) { $problems += 'the VM said no hello on its serial port' }
if (-not ($said -match '^NESTED counted')) { $problems += 'the VM did not finish counting' }
# under whpx the VM must not be running on QEMU's software CPU
if ($Accel -like 'whpx*' -and ($said -match 'TCGTCGTCGTCG')) { $problems += 'the VM ran on TCG, not on the hardware' }
foreach ($x in $problems) { "  PROBLEM: $x" }
if ($problems.Count) { "RESULT FAIL"; exit 1 }
"RESULT OK: a VM ran inside this Windows ($Accel, $secs s)"
exit 0
