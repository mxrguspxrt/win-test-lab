# setup.ps1 - win-test-lab BUILD phase (guest side).
#
# PowerShell 4.0 compatible (Windows Server 2012 R2 baseline). No PS5+ features.
#
# First run: started by the autounattend FirstLogonCommand from <CD>:\WTL\setup.ps1.
# Later runs: started at logon by HKLM Run "WTL-Build" -> C:\WTL\setup.ps1 (resume after reboot).
#
# State lives in C:\WTL\state.json, log in C:\WTL\build.log, events go to COM1 (SPEC serial protocol):
#   WTL <EVENT> [key=value ...] [| free text]

$ErrorActionPreference = 'Stop'

$WtlDir     = 'C:\WTL'
$StateFile  = Join-Path $WtlDir 'state.json'
$LogFile    = Join-Path $WtlDir 'build.log'
$PhaseFile  = Join-Path $WtlDir 'phase.txt'
$RunKey     = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Run'
$MaxRounds  = 30
$MaxFails   = 3
$WuCriteria = "IsInstalled=0 and Type='Software' and IsHidden=0"
$DefaultPassword = 'WinTest!2026'

$ScriptPath = $MyInvocation.MyCommand.Path
$ScriptDir  = Split-Path -Parent $ScriptPath

# ---------------------------------------------------------------- logging

$script:HasCom1 = $false
try { $script:HasCom1 = @([System.IO.Ports.SerialPort]::GetPortNames()) -contains 'COM1' } catch { $script:HasCom1 = $false }

function Write-SerialLine([string]$Line) {
    # Open COM1, write one line, close. Silent fallback if COM1 is unavailable.
    if (-not $script:HasCom1) { return }
    for ($attempt = 0; $attempt -lt 3; $attempt++) {
        $port = $null
        try {
            $port = New-Object System.IO.Ports.SerialPort('COM1', 115200, ([System.IO.Ports.Parity]::None), 8, ([System.IO.Ports.StopBits]::One))
            $port.Handshake = [System.IO.Ports.Handshake]::None
            $port.WriteTimeout = 3000
            $port.NewLine = "`r`n"
            $port.Open()
            $port.WriteLine($Line)
            $waited = 0
            while ($port.BytesToWrite -gt 0 -and $waited -lt 2000) { Start-Sleep -Milliseconds 20; $waited += 20 }
            return
        } catch {
            Start-Sleep -Milliseconds 250
        } finally {
            if ($port -ne $null) { try { $port.Close() } catch { }; try { $port.Dispose() } catch { } }
        }
    }
}

function Write-WtlLine([string]$Line) {
    $Line = ($Line -replace "[\r\n]+", ' ')
    $stamp = (Get-Date).ToString('yyyy-MM-dd HH:mm:ss')
    try { [System.IO.File]::AppendAllText($LogFile, "$stamp $Line`r`n") } catch { }
    try { Write-Host "$stamp $Line" } catch { }
    Write-SerialLine $Line
}

function ConvertTo-WtlValue([string]$Value) {
    # URL-escape values that contain anything outside a conservative safe set (spaces -> %20 etc.).
    if ($Value -match '^[A-Za-z0-9._~:\\/+,@()-]*$') { return $Value }
    return [Uri]::EscapeDataString($Value)
}

function Send-WtlEvent {
    param([string]$EventName, $Fields = $null, [string]$Text = $null)
    $line = "WTL $EventName"
    if ($Fields -ne $null) {
        foreach ($k in $Fields.Keys) { $line += ' ' + $k + '=' + (ConvertTo-WtlValue ([string]$Fields[$k])) }
    }
    if ($Text) { $line += ' | ' + $Text }
    Write-WtlLine $line
}

function Write-Log([string]$Text) { Send-WtlEvent -EventName 'LOG' -Text $Text }

function Set-Phase([string]$Text) {
    try { [System.IO.File]::WriteAllText($PhaseFile, $Text) } catch { }
}

# Heartbeat: a background job that emits "WTL LOG | still busy ..." every 5 minutes while a
# long blocking call (WU scan/download/install, DISM) runs in the main process.
$HeartbeatScript = {
    param($PhaseFile, $LogFile, $HasCom1)
    while ($true) {
        Start-Sleep -Seconds 300
        $phase = 'working'; $mins = 0
        try {
            if (Test-Path $PhaseFile) {
                $phase = ([System.IO.File]::ReadAllText($PhaseFile)).Trim()
                $mins = [int](((Get-Date) - (Get-Item $PhaseFile).LastWriteTime).TotalMinutes)
            }
        } catch { }
        $line = "WTL LOG | still busy: $phase (${mins} min)"
        try { [System.IO.File]::AppendAllText($LogFile, ((Get-Date).ToString('yyyy-MM-dd HH:mm:ss') + " $line`r`n")) } catch { }
        if ($HasCom1) {
            for ($i = 0; $i -lt 3; $i++) {
                $p = $null
                try {
                    $p = New-Object System.IO.Ports.SerialPort('COM1', 115200)
                    $p.WriteTimeout = 3000
                    $p.NewLine = "`r`n"
                    $p.Open()
                    $p.WriteLine($line)
                    $w = 0
                    while ($p.BytesToWrite -gt 0 -and $w -lt 2000) { Start-Sleep -Milliseconds 20; $w += 20 }
                    break
                } catch {
                    Start-Sleep -Milliseconds 300
                } finally {
                    if ($p -ne $null) { try { $p.Close() } catch { } }
                }
            }
        }
    }
}

$script:HeartbeatJob = $null
function Start-Heartbeat {
    if ($script:HeartbeatJob -ne $null) { return }
    try {
        $script:HeartbeatJob = Start-Job -ScriptBlock $HeartbeatScript -ArgumentList $PhaseFile, $LogFile, $script:HasCom1
    } catch {
        $script:HeartbeatJob = $null
        Write-Log "heartbeat job not available: $($_.Exception.Message)"
    }
}
function Stop-Heartbeat {
    if ($script:HeartbeatJob -eq $null) { return }
    try { Stop-Job -Job $script:HeartbeatJob -ErrorAction SilentlyContinue } catch { }
    try { Remove-Job -Job $script:HeartbeatJob -Force -ErrorAction SilentlyContinue } catch { }
    $script:HeartbeatJob = $null
}

# ---------------------------------------------------------------- helpers

function Invoke-Step([string]$Name, [scriptblock]$Block) {
    try { & $Block } catch { Write-Log "step '$Name' failed (ignored): $($_.Exception.Message)" }
}

function Set-RegValue([string]$Path, [string]$Name, $Value, [string]$Type = 'DWord') {
    if (-not (Test-Path $Path)) { New-Item -Path $Path -Force | Out-Null }
    New-ItemProperty -Path $Path -Name $Name -Value $Value -PropertyType $Type -Force | Out-Null
}

function Remove-RegValue([string]$Path, [string]$Name) {
    if (Test-Path $Path) { Remove-ItemProperty -Path $Path -Name $Name -ErrorAction SilentlyContinue }
}

function Invoke-Native([string]$Exe, [string[]]$Arguments, [switch]$Quiet) {
    # PS 4.0: with EAP=Stop, stderr output of a native exe redirected via 2>&1 becomes a
    # terminating error, so run native tools with a local EAP of Continue.
    $ErrorActionPreference = 'Continue'
    $out = & $Exe @Arguments 2>&1 | Out-String
    $code = $LASTEXITCODE
    if ($code -ne 0 -and -not $Quiet) { Write-Log ("$Exe " + ($Arguments -join ' ') + " exit=$code " + $out.Trim()) }
}

function Read-JsonFile([string]$Path) {
    return ([System.IO.File]::ReadAllText($Path) | ConvertFrom-Json)
}

function Get-State {
    $s = @{ round = 0; zeroStreak = 0; failStreak = 0; failedIds = @(); done = $false; started = (Get-Date).ToString('s') }
    if (Test-Path $StateFile) {
        try {
            $o = Read-JsonFile $StateFile
            foreach ($p in $o.PSObject.Properties) { $s[$p.Name] = $p.Value }
        } catch {
            Write-Log "state.json unreadable, starting fresh: $($_.Exception.Message)"
        }
    }
    $s.round      = [int]$s.round
    $s.zeroStreak = [int]$s.zeroStreak
    $s.failStreak = [int]$s.failStreak
    $s.failedIds  = @($s.failedIds | Where-Object { $_ })
    $s.done       = [bool]$s.done
    return $s
}

function Save-State($State) {
    $json = $State | ConvertTo-Json -Depth 4
    $tmp = "$StateFile.tmp"
    [System.IO.File]::WriteAllText($tmp, $json)
    [System.IO.File]::Copy($tmp, $StateFile, $true)
    Remove-Item $tmp -Force -ErrorAction SilentlyContinue
}

function Get-CdDrives {
    $list = @()
    foreach ($d in [System.IO.DriveInfo]::GetDrives()) {
        try { if ($d.DriveType -eq 'CDRom' -and $d.IsReady) { $list += $d.RootDirectory.FullName } } catch { }
    }
    return $list
}

function Clear-ReadOnly([string]$Dir) {
    # files copied from CD media keep the ReadOnly attribute
    Get-ChildItem -Path $Dir -Recurse -Force -ErrorAction SilentlyContinue |
        Where-Object { -not $_.PSIsContainer } |
        ForEach-Object { try { $_.IsReadOnly = $false } catch { } }
}

# ---------------------------------------------------------------- stage files

function Initialize-WtlDir {
    if (-not (Test-Path $WtlDir)) { New-Item -ItemType Directory -Path $WtlDir -Force | Out-Null }
    $src = $null
    if ($ScriptDir -and ((Resolve-Path $ScriptDir).Path.TrimEnd('\') -ne $WtlDir)) {
        $src = $ScriptDir
    } elseif (-not (Test-Path (Join-Path $WtlDir 'build.json')) -or -not (Test-Path (Join-Path $WtlDir 'agent.ps1'))) {
        foreach ($root in (Get-CdDrives)) {
            $cand = Join-Path $root 'WTL'
            if (Test-Path (Join-Path $cand 'build.json')) { $src = $cand; break }
        }
    }
    if ($src) {
        Write-Log "copying $src to $WtlDir"
        Get-ChildItem -Path $src -Force | ForEach-Object {
            Copy-Item -Path $_.FullName -Destination $WtlDir -Recurse -Force
        }
        Clear-ReadOnly $WtlDir
    }
    foreach ($f in @('setup.ps1', 'agent.ps1', 'build.json')) {
        if (-not (Test-Path (Join-Path $WtlDir $f))) { throw "missing $WtlDir\$f (WTL_BUILD CD not found?)" }
    }
}

# ---------------------------------------------------------------- system configuration (idempotent)

function Set-SystemConfig([string]$Password) {
    $winlogon = 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon'
    Invoke-Step 'autologon' {
        Set-RegValue $winlogon 'AutoAdminLogon' '1' 'String'
        Set-RegValue $winlogon 'DefaultUserName' 'Administrator' 'String'
        Set-RegValue $winlogon 'DefaultPassword' $Password 'String'
        Set-RegValue $winlogon 'DefaultDomainName' $env:COMPUTERNAME 'String'
        # unattend AutoLogon sets a decrementing AutoLogonCount; drop it -> permanent autologon
        Remove-RegValue $winlogon 'AutoLogonCount'
    }
    Invoke-Step 'firewall' { Invoke-Native 'netsh.exe' @('advfirewall', 'set', 'allprofiles', 'state', 'off') }
    Invoke-Step 'network profile private' {
        if (Get-Command Set-NetConnectionProfile -ErrorAction SilentlyContinue) {
            Get-NetConnectionProfile -ErrorAction SilentlyContinue | ForEach-Object {
                try { Set-NetConnectionProfile -InterfaceIndex $_.InterfaceIndex -NetworkCategory Private -ErrorAction Stop } catch { }
            }
        }
    }
    Invoke-Step 'server manager' {
        Set-RegValue 'HKLM:\SOFTWARE\Microsoft\ServerManager' 'DoNotOpenServerManagerAtLogon' 1
        Set-RegValue 'HKLM:\SOFTWARE\Policies\Microsoft\Windows\Server\ServerManager' 'DoNotOpenAtLogon' 1
        Set-RegValue 'HKCU:\Software\Microsoft\ServerManager' 'DoNotOpenServerManagerAtLogon' 1
        Set-RegValue 'HKCU:\Software\Microsoft\ServerManager' 'CheckedUnattendLaunchSetting' 0
        Invoke-Native 'schtasks.exe' @('/Change', '/TN', '\Microsoft\Windows\Server Manager\ServerManager', '/Disable') -Quiet
    }
    Invoke-Step 'IE ESC off' {
        foreach ($id in @('{A509B1A7-37EF-4b3f-8CFC-4F3A74704073}', '{A509B1A8-37EF-4b3f-8CFC-4F3A74704073}')) {
            $k = "HKLM:\SOFTWARE\Microsoft\Active Setup\Installed Components\$id"
            if (Test-Path $k) { Set-RegValue $k 'IsInstalled' 0 }
        }
    }
    Invoke-Step 'power' {
        Invoke-Native 'powercfg.exe' @('-setactive', '8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c')
        foreach ($t in @('monitor-timeout-ac', 'monitor-timeout-dc', 'standby-timeout-ac', 'standby-timeout-dc', 'disk-timeout-ac', 'disk-timeout-dc', 'hibernate-timeout-ac', 'hibernate-timeout-dc')) {
            Invoke-Native 'powercfg.exe' @('-change', "-$t", '0') -Quiet
        }
        Invoke-Native 'powercfg.exe' @('-h', 'off')
        Set-RegValue 'HKCU:\Control Panel\Desktop' 'ScreenSaveActive' '0' 'String'
    }
    Invoke-Step 'windows update policy' {
        $au = 'HKLM:\SOFTWARE\Policies\Microsoft\Windows\WindowsUpdate\AU'
        Set-RegValue $au 'NoAutoUpdate' 1
        Set-RegValue $au 'AUOptions' 1
        Set-RegValue $au 'NoAutoRebootWithLoggedOnUsers' 1
    }
    Invoke-Step 'shutdown event tracker' {
        $rel = 'HKLM:\SOFTWARE\Policies\Microsoft\Windows NT\Reliability'
        Set-RegValue $rel 'ShutdownReasonOn' 0
        Set-RegValue $rel 'ShutdownReasonUI' 0
    }
    Invoke-Step 'execution policy' {
        try { Set-ExecutionPolicy -ExecutionPolicy Bypass -Scope LocalMachine -Force -ErrorAction Stop } catch { }
    }
    Invoke-Step 'WER UI off' {
        Set-RegValue 'HKLM:\SOFTWARE\Microsoft\Windows\Windows Error Reporting' 'DontShowUI' 1
        Set-RegValue 'HKCU:\Software\Microsoft\Windows\Windows Error Reporting' 'DontShowUI' 1
    }
    Invoke-Step 'defender' {
        if (Get-Command Set-MpPreference -ErrorAction SilentlyContinue) {
            try { Set-MpPreference -DisableRealtimeMonitoring $true -ErrorAction Stop } catch { Write-Log "Set-MpPreference failed (tamper protection?): $($_.Exception.Message)" }
            if (Get-Command Add-MpPreference -ErrorAction SilentlyContinue) {
                $desk = [Environment]::GetFolderPath('Desktop')
                foreach ($p in @($desk, $WtlDir)) {
                    if ($p) { try { Add-MpPreference -ExclusionPath $p -ErrorAction Stop } catch { } }
                }
            }
        }
    }
    Invoke-Step 'run key WTL-Build' {
        Set-RegValue $RunKey 'WTL-Build' "powershell.exe -NoProfile -ExecutionPolicy Bypass -File $WtlDir\setup.ps1" 'String'
    }
}

# ---------------------------------------------------------------- Windows Update

function Start-WuService {
    foreach ($name in @('wuauserv', 'BITS')) {
        try {
            $svc = Get-WmiObject -Class Win32_Service -Filter "Name='$name'"
            if ($svc -and $svc.StartMode -eq 'Disabled') { Set-Service -Name $name -StartupType Manual }
            $s = Get-Service -Name $name
            if ($s.Status -ne 'Running') {
                Start-Service -Name $name
                $s.WaitForStatus('Running', [TimeSpan]::FromSeconds(60))
            }
        } catch {
            Write-Log "could not start service ${name}: $($_.Exception.Message)"
        }
    }
}

function Restart-WuService {
    try { Stop-Service -Name wuauserv -Force -ErrorAction SilentlyContinue } catch { }
    Start-Sleep -Seconds 5
    Start-WuService
}

function Test-RebootPending {
    $keys = @(
        'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\WindowsUpdate\Auto Update\RebootRequired',
        'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Component Based Servicing\RebootPending'
    )
    foreach ($k in $keys) { if (Test-Path $k) { return $true } }
    return $false
}

function Get-FailCount($State, [string]$Id) {
    return @($State.failedIds | Where-Object { $_ -eq $Id }).Count
}

function Invoke-WuRound($State) {
    # Returns @{ found; installed; reboot; failed }
    $n = $State.round
    Start-WuService

    $session = New-Object -ComObject Microsoft.Update.Session
    try { $session.ClientApplicationID = 'win-test-lab' } catch { }
    $searcher = $session.CreateUpdateSearcher()

    $result = $null
    $lastErr = $null
    for ($try = 1; $try -le 3; $try++) {
        Set-Phase "WU round $n search (attempt $try)"
        Write-Log "WU round ${n}: searching (attempt $try); first scan on old agents can take a long time"
        try {
            $result = $searcher.Search($WuCriteria)
            break
        } catch {
            $lastErr = $_.Exception.Message
            Write-Log "WU round ${n}: search attempt $try failed: $lastErr"
            if ($try -lt 3) { Restart-WuService; Start-Sleep -Seconds 30 }
        }
    }
    if ($result -eq $null) { throw "search failed 3 times: $lastErr" }

    $all = $result.Updates
    $candidates = @()
    for ($i = 0; $i -lt $all.Count; $i++) {
        $u = $all.Item($i)
        $id = [string]$u.Identity.UpdateID
        if ($u.BrowseOnly) { continue }
        try { if ($u.InstallationBehavior.CanRequestUserInput) { Write-Log "skip (needs user input): $($u.Title)"; continue } } catch { }
        if ((Get-FailCount $State $id) -ge 2) { Write-Log "skip (failed twice): $($u.Title)"; continue }
        $candidates += $u
    }
    Write-Log "WU round ${n}: search done, $($all.Count) returned, $($candidates.Count) applicable"
    $found = $candidates.Count
    if ($found -eq 0) {
        return @{ found = 0; installed = 0; reboot = (Test-RebootPending); failed = 0 }
    }

    # Updates that need exclusive handling (servicing stack etc.) go alone.
    $exclusive = @($candidates | Where-Object { try { $_.InstallationBehavior.Impact -eq 2 } catch { $false } })
    if ($exclusive.Count -gt 0) {
        Write-Log "WU round ${n}: exclusive update first: $($exclusive[0].Title)"
        $candidates = @($exclusive[0])
    }

    foreach ($u in $candidates) {
        if (-not $u.EulaAccepted) { try { $u.AcceptEula() } catch { Write-Log "AcceptEula failed: $($u.Title)" } }
    }

    $installed = 0; $failed = 0; $reboot = $false
    $batchSize = 10
    for ($start = 0; $start -lt $candidates.Count; $start += $batchSize) {
        $end = [Math]::Min($start + $batchSize, $candidates.Count) - 1
        $batchNo = [int]($start / $batchSize) + 1

        $dl = New-Object -ComObject Microsoft.Update.UpdateColl
        for ($j = $start; $j -le $end; $j++) { [void]$dl.Add($candidates[$j]) }

        Set-Phase "WU round $n download batch $batchNo ($($dl.Count) updates)"
        Write-Log "WU round ${n}: downloading batch $batchNo ($($dl.Count) updates)"
        try {
            $downloader = $session.CreateUpdateDownloader()
            try { $downloader.Priority = 3 } catch { }
            $downloader.Updates = $dl
            [void]$downloader.Download()
        } catch {
            Write-Log "WU round ${n}: download batch $batchNo error: $($_.Exception.Message)"
        }

        $inst = New-Object -ComObject Microsoft.Update.UpdateColl
        for ($j = 0; $j -lt $dl.Count; $j++) {
            $u = $dl.Item($j)
            if ($u.IsDownloaded) { [void]$inst.Add($u) }
            else { $failed++; $State.failedIds += [string]$u.Identity.UpdateID; Write-Log "not downloaded: $($u.Title)" }
        }
        if ($inst.Count -eq 0) { continue }

        Set-Phase "WU round $n install batch $batchNo ($($inst.Count) updates)"
        Write-Log "WU round ${n}: installing batch $batchNo ($($inst.Count) updates)"
        $installer = $session.CreateUpdateInstaller()
        try { $installer.ForceQuiet = $true } catch { }
        try { $installer.AllowSourcePrompts = $false } catch { }
        $installer.Updates = $inst
        $ires = $null
        try {
            $ires = $installer.Install()
        } catch {
            Write-Log "WU round ${n}: install batch $batchNo error: $($_.Exception.Message)"
        }
        if ($ires -ne $null) {
            if ($ires.RebootRequired) { $reboot = $true }
            for ($j = 0; $j -lt $inst.Count; $j++) {
                $u = $inst.Item($j)
                $code = -1
                try { $code = [int]$ires.GetUpdateResult($j).ResultCode } catch { }
                if ($code -eq 2 -or $code -eq 3) {
                    $installed++
                } else {
                    $failed++
                    $State.failedIds += [string]$u.Identity.UpdateID
                    Write-Log "install failed (result $code): $($u.Title)"
                }
            }
        } else {
            for ($j = 0; $j -lt $inst.Count; $j++) { $failed++; $State.failedIds += [string]$inst.Item($j).Identity.UpdateID }
        }
        Write-Log "WU round ${n}: batch $batchNo done, installed so far $installed, failed $failed"
    }

    if (Test-RebootPending) { $reboot = $true }
    return @{ found = $found; installed = $installed; reboot = $reboot; failed = $failed }
}

function Invoke-WuLoop($State) {
    while ($true) {
        if ($State.zeroStreak -ge 2) { Write-Log 'Windows Update: 0 updates found twice in a row, done'; return }
        if ($State.round -ge $MaxRounds) { Write-Log "Windows Update: reached $MaxRounds rounds, stopping"; return }
        if ($State.failStreak -ge $MaxFails) { Send-WtlEvent -EventName 'WU_GAVEUP' -Text "$MaxFails consecutive failed rounds"; return }

        $State.round = $State.round + 1
        Save-State $State
        $n = $State.round

        $r = $null
        Start-Heartbeat
        try {
            $r = Invoke-WuRound $State
        } catch {
            $r = $null
            Send-WtlEvent -EventName 'ERROR' -Text "WU round $n failed: $($_.Exception.Message)"
        } finally {
            Stop-Heartbeat
        }

        if ($r -eq $null) {
            $State.failStreak = $State.failStreak + 1
            Save-State $State
            Start-Sleep -Seconds 30
            continue
        }

        # a round that found updates but installed none (and needs no reboot) counts as failed
        if ($r.found -gt 0 -and $r.installed -eq 0 -and -not $r.reboot) {
            $State.failStreak = $State.failStreak + 1
        } else {
            $State.failStreak = 0
        }
        if ($r.found -eq 0) { $State.zeroStreak = $State.zeroStreak + 1 } else { $State.zeroStreak = 0 }
        Save-State $State

        $rb = 'false'
        if ($r.reboot) { $rb = 'true' }
        Write-WtlLine "WTL WU_ROUND $n found=$($r.found) installed=$($r.installed) reboot=$rb"

        if ($r.reboot) {
            Write-Log "rebooting after WU round $n (Run key WTL-Build resumes)"
            Start-Sleep -Seconds 2
            try { Restart-Computer -Force -ErrorAction Stop } catch { Write-Log "Restart-Computer failed: $($_.Exception.Message); using shutdown.exe"; Invoke-Native 'shutdown.exe' @('/r', '/t', '5', '/f') }
            Start-Sleep -Seconds 600
            exit 0
        }
    }
}

# ---------------------------------------------------------------- finish

function Complete-Build($State) {
    Remove-RegValue $RunKey 'WTL-Build'

    Start-Heartbeat
    try {
        Set-Phase 'DISM StartComponentCleanup'
        Write-Log 'cleanup: Dism /Online /Cleanup-Image /StartComponentCleanup (best effort)'
        Invoke-Step 'dism' { Invoke-Native 'dism.exe' @('/Online', '/Cleanup-Image', '/StartComponentCleanup', '/Quiet', '/NoRestart') }
        Write-Log 'cleanup: DISM finished'
    } finally {
        Stop-Heartbeat
    }
    Invoke-Step 'clear temp' {
        try { Stop-Service -Name wuauserv -Force -ErrorAction SilentlyContinue } catch { }
        foreach ($d in @($env:TEMP, "$env:SystemRoot\Temp", "$env:SystemRoot\SoftwareDistribution\Download")) {
            if ($d -and (Test-Path $d)) {
                Get-ChildItem -Path $d -Force -ErrorAction SilentlyContinue | ForEach-Object {
                    Remove-Item -Path $_.FullName -Recurse -Force -ErrorAction SilentlyContinue
                }
            }
        }
    }
    Remove-Item $PhaseFile -Force -ErrorAction SilentlyContinue

    $State.done = $true
    $State.finished = (Get-Date).ToString('s')
    Save-State $State

    # The agent autostart goes in LAST, after BUILD_DONE: Explorer may still be working through
    # this logon's Run keys, and an agent started now would hold COM1 open, so the BUILD_DONE
    # line (written by open-write-close) would be lost and the host would see only a shutdown.
    Send-WtlEvent -EventName 'BUILD_DONE'
    Set-RegValue $RunKey 'WTL-Agent' "powershell.exe -NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File $WtlDir\agent.ps1" 'String'
    try { [System.IO.File]::AppendAllText($LogFile, (Get-Date).ToString('yyyy-MM-dd HH:mm:ss') + " agent autostart installed (HKLM Run WTL-Agent)`r`n") } catch { }
    Start-Sleep -Seconds 2
    try { Stop-Computer -Force -ErrorAction Stop } catch { Invoke-Native 'shutdown.exe' @('/s', '/t', '0', '/f') }
}

# ---------------------------------------------------------------- main

function Invoke-Main {
    Initialize-WtlDir

    $mutex = New-Object System.Threading.Mutex($false, 'Global\WTL-Build')
    $owned = $false
    try { $owned = $mutex.WaitOne(0) } catch [System.Threading.AbandonedMutexException] { $owned = $true }
    if (-not $owned) { Write-Log 'setup.ps1 already running, exiting'; exit 0 }
    $script:Mutex = $mutex

    $state = Get-State
    if ($state.done) { Write-Log 'build already finished (state.json done=true), nothing to do'; return }

    $cfg = Read-JsonFile (Join-Path $WtlDir 'build.json')
    $password = [string]$cfg.password
    if (-not $password) { $password = $DefaultPassword }
    $doUpdates = $true
    if ($cfg.PSObject.Properties['updates'] -ne $null -and $cfg.updates -eq $false) { $doUpdates = $false }

    $osCaption = 'unknown'
    try { $osCaption = (Get-WmiObject -Class Win32_OperatingSystem).Caption.Trim() } catch { }
    Write-Log "setup.ps1 start: os=$($cfg.os) caption=$osCaption ps=$($PSVersionTable.PSVersion) round=$($state.round) updates=$doUpdates"

    Set-SystemConfig $password
    Write-Log 'system configuration applied'

    if ($doUpdates) {
        Invoke-WuLoop $state
    } else {
        Write-Log 'Windows Update skipped (build.json updates=false)'
    }

    Complete-Build $state
}

try {
    Invoke-Main
} catch {
    $msg = $_.Exception.Message
    try { $msg = "$msg (line $($_.InvocationInfo.ScriptLineNumber))" } catch { }
    Stop-Heartbeat
    Write-WtlLine ("WTL BUILD_FAIL | " + $msg)
    # Do NOT shut down: leave the VM up so the host can take a screenshot.
    while ($true) { Start-Sleep -Seconds 3600 }
}
