# agent.ps1 - win-test-lab TEST phase agent (guest side).
#
# PowerShell 4.0 / .NET 4.5 compatible. No PS5+ features.
# Started at every logon by HKLM Run "WTL-Agent":
#   powershell.exe -NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File C:\WTL\agent.ps1
#
# Looks for \JOB.JSON + \PAYLOAD.ZIP on a CD drive (job.iso, vol id WTL_JOB), copies the zip to the
# Desktop, extracts it, starts the EXE and reports over COM1 (or, with no COM1, the virtio-serial
# port QEMU appends to the same host log) using the SPEC serial protocol:
#   WTL <EVENT> [key=value ...] [| free text]

$ErrorActionPreference = 'Stop'

$WtlDir       = 'C:\WTL'
$LogFile      = Join-Path $WtlDir 'agent.log'
$JobPollSecs  = 600
$DefaultTimeout = 900

Add-Type -AssemblyName System.IO.Compression.FileSystem

# ---------------------------------------------------------------- serial + log

$script:Port = $null
$script:PortRetryAt = [DateTime]::MinValue

function Open-Serial {
    if ($script:Port -ne $null -and $script:Port.IsOpen) { return $true }
    if ([DateTime]::Now -lt $script:PortRetryAt) { return $false }
    if ($script:Port -ne $null) { try { $script:Port.Dispose() } catch { }; $script:Port = $null }
    try {
        $p = New-Object System.IO.Ports.SerialPort('COM1', 115200, ([System.IO.Ports.Parity]::None), 8, ([System.IO.Ports.StopBits]::One))
        $p.Handshake = [System.IO.Ports.Handshake]::None
        $p.WriteTimeout = 3000
        $p.NewLine = "`r`n"
        $p.Open()
        $script:Port = $p
        return $true
    } catch {
        $script:Port = $null
        $script:PortRetryAt = [DateTime]::Now.AddSeconds(10)
        return $false
    }
}

# Without COM1 (Windows on Arm has no driver for QEMU's UART) every line goes to a virtio-serial
# port (virtio-win's vioserial) that QEMU appends to the host's serial log file. The port stays
# open, like COM1.
$WtlChannel = '\\.\Global\org.wintestlab.0'
$script:HasCom1 = $true
try { $script:HasCom1 = @([System.IO.Ports.SerialPort]::GetPortNames()) -contains 'COM1' } catch { }
$script:Chan = $null

# .NET's FileStream refuses device paths (\\.\Global\...): open the port with CreateFile and
# wrap the handle. Returns $null when the port is not there (yet).
function Open-WtlChannel {
    if (-not ('WtlNative.Chan' -as [type])) {
        Add-Type -Namespace WtlNative -Name Chan -MemberDefinition @'
[DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
public static extern Microsoft.Win32.SafeHandles.SafeFileHandle CreateFile(string name, uint access, uint share, IntPtr sa, uint disposition, uint flags, IntPtr template);
'@
    }
    # GENERIC_WRITE, no sharing, OPEN_EXISTING
    $h = [WtlNative.Chan]::CreateFile($WtlChannel, 0x40000000, 0, [IntPtr]::Zero, 3, 0, [IntPtr]::Zero)
    if ($h.IsInvalid) { $h.Dispose(); return $null }
    return (New-Object System.IO.FileStream($h, [System.IO.FileAccess]::Write, 1))
}

# Only administrators and SYSTEM may open the port (a standard user gets access denied, error 5):
# a lab user's agent hands its lines to the SYSTEM worker (labsvc.ps1) instead, as files in
# C:\WTL\chan, which it forwards in name order. Without a worker yet (the base-image build) or
# as an administrator, the port is written directly.
$WtlChanSpool = Join-Path $WtlDir 'chan'
$script:ChanSeq = 0

function Write-Channel([string]$Line) {
    $bytes = [System.Text.Encoding]::UTF8.GetBytes($Line + "`r`n")
    $until = (Get-Date).AddSeconds(60)
    do {
        try {
            if ($script:Chan -eq $null) {
                $script:Chan = Open-WtlChannel
                if ($script:Chan -eq $null) { throw 'no channel' }
            }
            $script:Chan.Write($bytes, 0, $bytes.Length)
            $script:Chan.Flush()
            return
        } catch {
            if ($script:Chan -ne $null) { try { $script:Chan.Close() } catch { } }
            $script:Chan = $null
        }
        if (Test-Path -LiteralPath $WtlChanSpool) {
            # <ticks>-<pid>-<seq>.line: sortable, unique per writer; .tmp until complete
            $script:ChanSeq++
            $name = '{0:D19}-{1}-{2:D6}' -f [DateTime]::UtcNow.Ticks, $PID, $script:ChanSeq
            $tmp = Join-Path $WtlChanSpool ($name + '.tmp')
            try {
                [System.IO.File]::WriteAllBytes($tmp, $bytes)
                [System.IO.File]::Move($tmp, (Join-Path $WtlChanSpool ($name + '.line')))
                return
            } catch { }
        }
        Start-Sleep -Milliseconds 500
    } while ((Get-Date) -lt $until)
}

function Write-Serial([string]$Line) {
    if (-not $script:HasCom1) { Write-Channel $Line; return }
    for ($attempt = 0; $attempt -lt 2; $attempt++) {
        if (-not (Open-Serial)) { return }
        try {
            $script:Port.WriteLine($Line)
            return
        } catch {
            # reopen on failure
            try { $script:Port.Close() } catch { }
            try { $script:Port.Dispose() } catch { }
            $script:Port = $null
            $script:PortRetryAt = [DateTime]::MinValue
        }
    }
}

function Write-WtlLine([string]$Line) {
    $Line = ($Line -replace "[\r\n]+", ' ')
    $stamp = (Get-Date).ToString('yyyy-MM-dd HH:mm:ss')
    try {
        if (-not (Test-Path $WtlDir)) { New-Item -ItemType Directory -Path $WtlDir -Force | Out-Null }
        [System.IO.File]::AppendAllText($LogFile, "$stamp $Line`r`n")
    } catch { }
    Write-Serial $Line
}

function ConvertTo-WtlValue([string]$Value) {
    if ($Value -match '^[A-Za-z0-9._~:\\/+,@()-]+$') { return $Value }
    return [Uri]::EscapeDataString($Value)
}

# $Pairs: array of @(key, value) pairs, keeps order (hashtables are unordered in PS 4.0 output)
function Send-WtlEvent([string]$EventName, [object[]]$Pairs = @(), [string]$Text = '') {
    $line = "WTL $EventName"
    for ($i = 0; $i + 1 -lt $Pairs.Count; $i += 2) {
        $line += ' ' + [string]$Pairs[$i] + '=' + (ConvertTo-WtlValue ([string]$Pairs[$i + 1]))
    }
    if ($Text) { $line += ' | ' + $Text }
    Write-WtlLine $line
}

function Write-Log([string]$Text) { Send-WtlEvent 'LOG' @() $Text }

# ---------------------------------------------------------------- helpers

function Get-CdDrives {
    $list = @()
    foreach ($d in [System.IO.DriveInfo]::GetDrives()) {
        try { if ($d.DriveType -eq 'CDRom' -and $d.IsReady) { $list += $d.RootDirectory.FullName } } catch { }
    }
    return $list
}

function Find-JobDrive([int]$WaitSecs = $JobPollSecs) {
    $deadline = [DateTime]::Now.AddSeconds($WaitSecs)
    while ($true) {
        foreach ($root in (Get-CdDrives)) {
            if (Test-Path -LiteralPath (Join-Path $root 'JOB.JSON')) { return $root }
        }
        if ([DateTime]::Now -ge $deadline) { return $null }
        Start-Sleep -Seconds 2
    }
}

# AD lab mode: a CD (the control disc the host's `lab` writes) with \LAB\lab.ps1 + \LAB\lab.json
function Find-LabDir {
    for ($i = 0; $i -lt 3; $i++) {
        foreach ($root in (Get-CdDrives)) {
            $d = Join-Path $root 'LAB'
            if ((Test-Path -LiteralPath (Join-Path $d 'lab.ps1')) -and (Test-Path -LiteralPath (Join-Path $d 'lab.json'))) { return $d }
        }
        Start-Sleep -Seconds 2
    }
    return $null
}

function Find-CaseInsensitive([string]$Dir, [string]$Name) {
    $f = Get-ChildItem -LiteralPath $Dir -Force -ErrorAction SilentlyContinue | Where-Object { $_.Name -eq $Name } | Select-Object -First 1
    if ($f) { return $f.FullName }
    return $null
}

function Test-TcpPort([int]$PortNo) {
    $c = New-Object System.Net.Sockets.TcpClient
    try {
        $ar = $c.BeginConnect('127.0.0.1', $PortNo, $null, $null)
        if (-not $ar.AsyncWaitHandle.WaitOne(1000, $false)) { return $false }
        $c.EndConnect($ar)
        return $c.Connected
    } catch {
        return $false
    } finally {
        try { $c.Close() } catch { }
    }
}

function Get-ExitCodeSafe($Proc) {
    try { $Proc.WaitForExit(); return $Proc.ExitCode } catch { return -1 }
}

function Wait-Forever {
    # keep the agent (and serial port) alive so the host can screenshot / inspect
    while ($true) { Start-Sleep -Seconds 60 }
}

function Watch-Process($Proc) {
    while (-not $Proc.HasExited) { Start-Sleep -Milliseconds 500 }
    Send-WtlEvent 'PROC_EXIT' @('code', (Get-ExitCodeSafe $Proc))
}

# No "program can't start / missing DLL" or crash dialogs: they would block an unattended run.
# The error mode is inherited by child processes; WER UI is switched off machine-wide.
function Disable-ErrorDialogs {
    try {
        Add-Type -Namespace WTL -Name Native -MemberDefinition '[DllImport("kernel32.dll")] public static extern uint SetErrorMode(uint mode);' -ErrorAction Stop
        # SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX | SEM_NOOPENFILEERRORBOX
        $null = [WTL.Native]::SetErrorMode(0x8003)
    } catch { Write-Log ('SetErrorMode failed: ' + $_.Exception.Message) }
    try {
        $wer = 'HKLM:\SOFTWARE\Microsoft\Windows\Windows Error Reporting'
        if (-not (Test-Path $wer)) { New-Item -Path $wer -Force | Out-Null }
        Set-ItemProperty -Path $wer -Name 'DontShowUI' -Value 1 -Type DWord -Force
    } catch { }
}

# read a file even if some leftover process still has it open for writing
function Read-SharedBytes([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path)) { return (New-Object byte[] 0) }
    $fs = $null
    try {
        $fs = New-Object System.IO.FileStream($Path, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, ([System.IO.FileShare]::ReadWrite -bor [System.IO.FileShare]::Delete))
        $ms = New-Object System.IO.MemoryStream
        $fs.CopyTo($ms)
        return $ms.ToArray()
    } catch {
        return (New-Object byte[] 0)
    } finally {
        if ($fs -ne $null) { $fs.Dispose() }
    }
}

# send a captured stream as base64 chunks:  WTL OUTPUT stream=stdout seq=N | <base64>
$OutputCapBytes = 262144
$OutputChunk    = 768
function Send-Output([string]$Stream, [string]$Path) {
    $bytes = Read-SharedBytes $Path
    $total = $bytes.Length
    $n = [Math]::Min($total, $OutputCapBytes)
    $seq = 0
    for ($off = 0; $off -lt $n; $off += $OutputChunk) {
        $len = [Math]::Min($OutputChunk, $n - $off)
        Send-WtlEvent 'OUTPUT' @('stream', $Stream, 'seq', $seq) ([Convert]::ToBase64String($bytes, $off, $len))
        $seq++
    }
    Send-WtlEvent 'OUTPUT_END' @('stream', $Stream, 'bytes', $total, 'sent', $n, 'truncated', ([int]($total -gt $n)))
}

function Stop-Tree($Proc) {
    try { & taskkill.exe /T /F /PID $Proc.Id 2>&1 | Out-Null } catch { }
    try { if (-not $Proc.HasExited) { $Proc.Kill() } } catch { }
}

# mode "exit": run the program to completion with stdout/stderr captured, then report.
# $Leave (job.leave_running): at the deadline report the output so far + PROC_RUNNING and let the
# program run on (GUI programs, services); PROC_EXIT follows whenever it ends.
function Invoke-RunToExit([string]$ExePath, [string]$ExeArgs, [string]$WorkDir, [int]$Timeout, [string]$OutDir, [bool]$Leave = $false) {
    if (-not (Test-Path -LiteralPath $OutDir)) { New-Item -ItemType Directory -Path $OutDir -Force | Out-Null }
    $outFile = Join-Path $OutDir 'stdout.txt'
    $errFile = Join-Path $OutDir 'stderr.txt'
    $inFile  = Join-Path $OutDir 'stdin.txt'
    foreach ($f in @($outFile, $errFile)) { if (Test-Path -LiteralPath $f) { Remove-Item -LiteralPath $f -Force } }
    [System.IO.File]::WriteAllText($inFile, '')   # programs that read stdin get EOF instead of hanging

    $ext = [System.IO.Path]::GetExtension($ExePath).ToLowerInvariant()
    $file = $ExePath
    $argLine = $ExeArgs
    if ($ext -eq '.ps1') {
        $file = 'powershell.exe'
        $argLine = '-NoProfile -ExecutionPolicy Bypass -File "' + $ExePath + '"'
        if ($ExeArgs) { $argLine += ' ' + $ExeArgs }
    } elseif ($ext -eq '.bat' -or $ext -eq '.cmd') {
        $file = $env:ComSpec
        $argLine = '/d /c ""' + $ExePath + '"'
        if ($ExeArgs) { $argLine += ' ' + $ExeArgs }
        $argLine += '"'
    }
    $sp = @{ FilePath = $file; WorkingDirectory = $WorkDir; PassThru = $true; NoNewWindow = $true
             RedirectStandardOutput = $outFile; RedirectStandardError = $errFile; RedirectStandardInput = $inFile }
    if ($argLine) { $sp['ArgumentList'] = $argLine }
    $proc = $null
    try {
        $proc = Start-Process @sp
    } catch {
        Send-WtlEvent 'LAUNCH_FAIL' @() $_.Exception.Message
        return
    }
    if ($proc -eq $null) { Send-WtlEvent 'LAUNCH_FAIL' @() 'Start-Process returned no process'; return }
    try { $null = $proc.Handle } catch { }
    Send-WtlEvent 'LAUNCHED' @('pid', $proc.Id, 'exe', $ExePath, 'mode', 'exit')

    $t0 = [DateTime]::Now
    # in exit mode job.timeout is the program's own run limit (host --run-timeout)
    $deadline = $t0.AddSeconds([Math]::Max(5, $Timeout))
    $timedOut = $false
    while (-not $proc.HasExited) {
        if ([DateTime]::Now -ge $deadline) { $timedOut = $true; break }
        Start-Sleep -Milliseconds 250
    }
    if ($timedOut -and $Leave) {
        $secs = [Math]::Round(([DateTime]::Now - $t0).TotalSeconds, 1)
        Send-Output 'stdout' $outFile
        Send-Output 'stderr' $errFile
        Send-WtlEvent 'PROC_RUNNING' @('pid', $proc.Id, 'secs', $secs) 'program still running, left running'
        while (-not $proc.HasExited) { Start-Sleep -Milliseconds 500 }
        Send-WtlEvent 'PROC_EXIT' @('code', (Get-ExitCodeSafe $proc), 'secs', ([Math]::Round(([DateTime]::Now - $t0).TotalSeconds, 1)))
        return
    }
    if ($timedOut) {
        Stop-Tree $proc
        Start-Sleep -Seconds 1
    }
    $secs = [Math]::Round(([DateTime]::Now - $t0).TotalSeconds, 1)
    Send-Output 'stdout' $outFile
    Send-Output 'stderr' $errFile
    if ($timedOut) {
        Send-WtlEvent 'PROC_TIMEOUT' @('secs', $secs) 'program still running at the deadline, killed'
    } else {
        Send-WtlEvent 'PROC_EXIT' @('code', (Get-ExitCodeSafe $proc), 'secs', $secs)
    }
}

# ---------------------------------------------------------------- main

function Invoke-Agent {
    $caption = 'unknown'; $build = 0
    try {
        $osw = Get-WmiObject -Class Win32_OperatingSystem
        $caption = ([string]$osw.Caption).Trim()
        $build = [string]$osw.BuildNumber
    } catch {
        $build = [Environment]::OSVersion.Version.Build
    }
    Send-WtlEvent 'AGENT_START' @('os', $caption, 'build', $build)
    Disable-ErrorDialogs

    $jobWait = $JobPollSecs
    $labDir = Find-LabDir
    if ($labDir) {
        . (Join-Path $labDir 'lab.ps1')
        $labResult = Invoke-Lab $labDir
        if ($labResult -ne 'ready') { Wait-Forever }   # rebooting, or LAB_FAIL already reported
        $jobWait = 0   # a lab machine has no job disc: don't wait for one
    }

    Send-WtlEvent 'JOB_WAIT'
    $root = Find-JobDrive $jobWait
    if ($root -eq $null) {
        Write-Log 'no job'
        if ($labDir) { Wait-Forever }
        exit 0   # normal for manual boots: exit quietly
    }
    Send-WtlEvent 'JOB_FOUND' @('drive', $root.TrimEnd('\'))

    $jobPath = Find-CaseInsensitive $root 'JOB.JSON'
    $job = [System.IO.File]::ReadAllText($jobPath) | ConvertFrom-Json

    $zipName = [string]$job.zip_name
    if (-not $zipName) { $zipName = 'PAYLOAD.ZIP' }
    $zipName = [System.IO.Path]::GetFileName($zipName)
    $exeRel  = [string]$job.exe
    $exeArgs = [string]$job.args
    $timeout = $DefaultTimeout
    if ($job.timeout) { try { $timeout = [int]$job.timeout } catch { } }
    $waitPorts = @()
    if ($job.wait_ports -ne $null) { $waitPorts = @($job.wait_ports | Where-Object { $_ -ne $null } | ForEach-Object { [int]$_ }) }
    Write-Log ("job run_id=" + $job.run_id + " zip=" + $zipName + " exe=" + $exeRel + " ports=" + ($waitPorts -join ','))

    $payload = Find-CaseInsensitive $root 'PAYLOAD.ZIP'
    if (-not $payload) {
        Write-Log 'job has no PAYLOAD.ZIP, nothing to launch'
        return
    }
    if (-not $exeRel) {
        Write-Log 'job has no exe, nothing to launch'
        return
    }

    $desktop = [Environment]::GetFolderPath('Desktop')
    if (-not $desktop) { $desktop = Join-Path $env:USERPROFILE 'Desktop' }
    if (-not (Test-Path -LiteralPath $desktop)) { New-Item -ItemType Directory -Path $desktop -Force | Out-Null }

    $zipDest = Join-Path $desktop $zipName
    Copy-Item -LiteralPath $payload -Destination $zipDest -Force
    try { (Get-Item -LiteralPath $zipDest).IsReadOnly = $false } catch { }
    Send-WtlEvent 'COPIED' @('path', $zipDest)

    $stem = [System.IO.Path]::GetFileNameWithoutExtension($zipName)
    if (-not $stem) { $stem = 'payload' }
    $extractDir = Join-Path $desktop $stem
    if (Test-Path -LiteralPath $extractDir) { Remove-Item -LiteralPath $extractDir -Recurse -Force }
    [System.IO.Compression.ZipFile]::ExtractToDirectory($zipDest, $extractDir)
    Send-WtlEvent 'EXTRACTED' @('dir', $extractDir)

    # resolve EXE: relative path first, then first recursive match by file name
    $exePath = $null
    $rel = $exeRel.Replace('/', '\').TrimStart('\')
    $cand = Join-Path $extractDir $rel
    if (Test-Path -LiteralPath $cand -PathType Leaf) { $exePath = (Resolve-Path -LiteralPath $cand).Path }
    if (-not $exePath) {
        $leaf = [System.IO.Path]::GetFileName($rel)
        $m = Get-ChildItem -LiteralPath $extractDir -Recurse -Force -ErrorAction SilentlyContinue |
            Where-Object { -not $_.PSIsContainer -and $_.Name -eq $leaf } | Select-Object -First 1
        if ($m) { $exePath = $m.FullName }
    }
    if (-not $exePath) {
        Send-WtlEvent 'LAUNCH_FAIL' @() "exe not found in zip: $exeRel"
        return
    }

    $workDir = Split-Path -Parent $exePath
    $mode = [string]$job.mode
    if ($mode -eq 'exit') {
        $leave = $false
        if ($job.leave_running) { $leave = $true }
        Invoke-RunToExit $exePath $exeArgs $workDir $timeout (Join-Path $WtlDir 'run') $leave
        return
    }
    $ext = [System.IO.Path]::GetExtension($exePath).ToLowerInvariant()
    $proc = $null
    try {
        if ($ext -eq '.ps1') {
            $argLine = '-NoProfile -ExecutionPolicy Bypass -File "' + $exePath + '"'
            if ($exeArgs) { $argLine += ' ' + $exeArgs }
            $proc = Start-Process -FilePath 'powershell.exe' -ArgumentList $argLine -WorkingDirectory $workDir -PassThru
        } elseif ($exeArgs) {
            # .exe/.bat/.cmd: Start-Process uses ShellExecute, which handles batch files
            $proc = Start-Process -FilePath $exePath -ArgumentList $exeArgs -WorkingDirectory $workDir -PassThru
        } else {
            $proc = Start-Process -FilePath $exePath -WorkingDirectory $workDir -PassThru
        }
    } catch {
        Send-WtlEvent 'LAUNCH_FAIL' @() $_.Exception.Message
        return
    }
    if ($proc -eq $null) {
        Send-WtlEvent 'LAUNCH_FAIL' @() 'Start-Process returned no process'
        return
    }
    # touching .Handle makes .NET cache the process handle so ExitCode is available later
    try { $null = $proc.Handle } catch { }
    Send-WtlEvent 'LAUNCHED' @('pid', $proc.Id, 'exe', $exePath)

    $deadline = [DateTime]::Now.AddSeconds($timeout)
    if ($waitPorts.Count -eq 0) {
        $readyAt = [DateTime]::Now.AddSeconds(3)
        while ([DateTime]::Now -lt $readyAt -and -not $proc.HasExited) { Start-Sleep -Milliseconds 200 }
        if ($proc.HasExited) {
            Send-WtlEvent 'PROC_EXIT' @('code', (Get-ExitCodeSafe $proc))
            return
        }
        Send-WtlEvent 'READY'
    } else {
        foreach ($wp in $waitPorts) {
            $open = $false
            while ($true) {
                if ($proc.HasExited) {
                    Send-WtlEvent 'PROC_EXIT' @('code', (Get-ExitCodeSafe $proc))
                    return
                }
                if (Test-TcpPort $wp) { $open = $true; break }
                if ([DateTime]::Now -ge $deadline) { break }
                Start-Sleep -Milliseconds 500
            }
            if (-not $open) {
                Send-WtlEvent 'ERROR' @() "timeout waiting for port $wp"
                Watch-Process $proc
                return
            }
            Send-WtlEvent 'READY' @('port', $wp)
        }
    }

    Watch-Process $proc
}

# single instance per session
$mutex = New-Object System.Threading.Mutex($false, 'Global\WTL-Agent')
$owned = $false
try { $owned = $mutex.WaitOne(0) } catch [System.Threading.AbandonedMutexException] { $owned = $true }
if (-not $owned) { exit 0 }

try {
    Invoke-Agent
} catch {
    $msg = $_.Exception.Message
    try { $msg = "$msg (line $($_.InvocationInfo.ScriptLineNumber))" } catch { }
    Send-WtlEvent 'ERROR' @() $msg
}
Wait-Forever
