# agent.ps1 (2008) - win-test-lab logon agent for Windows Server 2008 R2 (guest side).
#
# PowerShell 2.0 / .NET 2.0 compatible: what Server 2008 R2 ships (tests/ps_lint.py --ps2).
# Started at every logon by HKLM Run "WTL-Agent":
#   powershell.exe -NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File C:\WTL\agent.ps1
#
# The lab flow of guest/agent.ps1, without its older job mode (that needs .NET 4.5's zip
# classes): owns COM1, finds the control disc (\LAB\lab.ps1 + \LAB\lab.json), dot-sources
# lab.ps1 and runs Invoke-Lab. Reports with the SPEC serial protocol:
#   WTL <EVENT> [key=value ...] [| free text]

$ErrorActionPreference = 'Stop'

$WtlDir  = 'C:\WTL'
$LogFile = Join-Path $WtlDir 'agent.log'

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

function Write-Serial([string]$Line) {
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

# $Pairs: array of @(key, value) pairs, keeps order
function Send-WtlEvent([string]$EventName, [object[]]$Pairs = @(), [string]$Text = '') {
    $line = "WTL $EventName"
    for ($i = 0; $i + 1 -lt $Pairs.Count; $i += 2) {
        $line += ' ' + [string]$Pairs[$i] + '=' + (ConvertTo-WtlValue ([string]$Pairs[$i + 1]))
    }
    if ($Text) { $line += ' | ' + $Text }
    Write-WtlLine $line
}

function Write-Log([string]$Text) { Send-WtlEvent 'LOG' @() $Text }

# ---------------------------------------------------------------- helpers (lab.ps1 uses them)

function Get-CdDrives {
    $list = @()
    foreach ($d in [System.IO.DriveInfo]::GetDrives()) {
        try { if ($d.DriveType -eq 'CDRom' -and $d.IsReady) { $list += $d.RootDirectory.FullName } } catch { }
    }
    return $list
}

# the control disc the host's `wtl` writes: \LAB\lab.ps1 + \LAB\lab.json
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

function Get-ExitCodeSafe($Proc) {
    try { $Proc.WaitForExit(); return $Proc.ExitCode } catch { return -1 }
}

function Wait-Forever {
    # keep the agent (and serial port) alive so the host can screenshot / inspect
    while ($true) { Start-Sleep -Seconds 60 }
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

# read a file even if some leftover process still has it open for writing (.NET 2.0 has no
# Stream.CopyTo)
function Read-SharedBytes([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path)) { return ,(New-Object byte[] 0) }
    $fs = $null
    try {
        $fs = New-Object System.IO.FileStream($Path, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, ([System.IO.FileShare]::ReadWrite -bor [System.IO.FileShare]::Delete))
        $ms = New-Object System.IO.MemoryStream
        $buf = New-Object byte[] 65536
        while (($n = $fs.Read($buf, 0, $buf.Length)) -gt 0) { $ms.Write($buf, 0, $n) }
        return ,($ms.ToArray())
    } catch {
        return ,(New-Object byte[] 0)
    } finally {
        if ($fs -ne $null) { $fs.Close() }
    }
}

# send a captured stream as base64 chunks:  WTL OUTPUT stream=step1out seq=N | <base64>
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

    $labDir = Find-LabDir
    if (-not $labDir) {
        Write-Log 'no lab control disc'
        exit 0   # a manual boot of the base image: exit quietly
    }
    . (Join-Path $labDir 'lab.ps1')
    [void](Invoke-Lab $labDir)
    # ready, rebooting or LAB_FAIL already reported: keep COM1 for the host
    Wait-Forever
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
