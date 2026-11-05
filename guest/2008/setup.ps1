# setup.ps1 (2008) - win-test-lab BUILD phase for Windows Server 2008 R2 (guest side).
#
# PowerShell 2.0 / .NET 2.0 compatible: what Server 2008 R2 ships. No ConvertFrom-Json /
# ConvertTo-Json, no [ordered], no Windows 8+ cmdlets (tests/ps_lint.py --ps2 checks a subset).
#
# 2008 R2 is not installed from an ISO: its base image is Microsoft's VHD, already installed
# (docs/ISOS.md). The host (`wtl build-image 2008`) boots it, logs on as its Administrator by
# typing over QMP, and starts this script from the WTL_BUILD disc (<CD>:\WTL\setup.ps1). It
# copies the guest scripts to C:\WTL, gives the Administrator the build's password, turns on
# autologon, renames the computer, configures the machine like the other base images (firewall
# off, no Server Manager at logon, power), installs the agent autostart after BUILD_DONE and
# shuts down. No Windows Update: the image came with its updates.
#
# Log in C:\WTL\build.log, events go to COM1 (SPEC serial protocol):
#   WTL <EVENT> [key=value ...] [| free text]

$ErrorActionPreference = 'Stop'

$WtlDir    = 'C:\WTL'
$LogFile   = Join-Path $WtlDir 'build.log'
$RunKey    = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Run'
$DefaultPassword = 'WinTest!2026'

$ScriptPath = $MyInvocation.MyCommand.Path
$ScriptDir  = Split-Path -Parent $ScriptPath

# ---------------------------------------------------------------- logging

function Write-SerialLine([string]$Line) {
    # Open COM1, write one line, close (the agent keeps it open later; it is not running yet).
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
    try {
        if (-not (Test-Path $WtlDir)) { New-Item -ItemType Directory -Path $WtlDir -Force | Out-Null }
        [System.IO.File]::AppendAllText($LogFile, "$stamp $Line`r`n")
    } catch { }
    try { Write-Host "$stamp $Line" } catch { }
    Write-SerialLine $Line
}

function ConvertTo-WtlValue([string]$Value) {
    # URL-escape values that contain anything outside a conservative safe set (spaces -> %20 etc.).
    if ($Value -match '^[A-Za-z0-9._~:\\/+,@()-]*$') { return $Value }
    return [Uri]::EscapeDataString($Value)
}

function Send-WtlEvent([string]$EventName, [object[]]$Pairs = @(), [string]$Text = '') {
    $line = "WTL $EventName"
    for ($i = 0; $i + 1 -lt $Pairs.Count; $i += 2) {
        $line += ' ' + [string]$Pairs[$i] + '=' + (ConvertTo-WtlValue ([string]$Pairs[$i + 1]))
    }
    if ($Text) { $line += ' | ' + $Text }
    Write-WtlLine $line
}

function Write-Log([string]$Text) { Send-WtlEvent 'LOG' @() $Text }

# ---------------------------------------------------------------- JSON (PowerShell 2.0 has none)

# A small JSON reader: objects become hashtables (read them as $o.key), arrays object[],
# numbers int / long / double, true / false bool, null $null.
function ConvertFrom-WtlJson([string]$Text) {
    $script:JsonText = $Text
    $script:JsonPos = 0
    $v = Read-WtlJsonValue
    Skip-WtlJsonSpace
    if ($script:JsonPos -lt $script:JsonText.Length) { throw "JSON: unexpected text at offset $($script:JsonPos)" }
    return ,$v
}

function Skip-WtlJsonSpace {
    $t = $script:JsonText
    while ($script:JsonPos -lt $t.Length -and [char]::IsWhiteSpace($t[$script:JsonPos])) { $script:JsonPos++ }
}

function Read-WtlJsonString {
    $t = $script:JsonText
    $script:JsonPos++   # the opening quote
    $sb = New-Object System.Text.StringBuilder
    while ($true) {
        if ($script:JsonPos -ge $t.Length) { throw 'JSON: unterminated string' }
        $c = $t[$script:JsonPos]
        $script:JsonPos++
        if ($c -eq '"') { break }
        if ($c -ne '\') { [void]$sb.Append($c); continue }
        $e = [string]$t[$script:JsonPos]
        $script:JsonPos++
        switch -CaseSensitive ($e) {
            'n' { [void]$sb.Append("`n") }
            'r' { [void]$sb.Append("`r") }
            't' { [void]$sb.Append("`t") }
            'b' { [void]$sb.Append([char]8) }
            'f' { [void]$sb.Append([char]12) }
            'u' { [void]$sb.Append([char][Convert]::ToInt32($t.Substring($script:JsonPos, 4), 16)); $script:JsonPos += 4 }
            default { [void]$sb.Append($e) }   # \" \\ \/
        }
    }
    return $sb.ToString()
}

function Read-WtlJsonValue {
    Skip-WtlJsonSpace
    $t = $script:JsonText
    if ($script:JsonPos -ge $t.Length) { throw 'JSON: unexpected end' }
    $c = [string]$t[$script:JsonPos]
    if ($c -eq '{') {
        $h = @{}
        $script:JsonPos++
        Skip-WtlJsonSpace
        if ([string]$t[$script:JsonPos] -eq '}') { $script:JsonPos++; return $h }
        while ($true) {
            Skip-WtlJsonSpace
            if ([string]$t[$script:JsonPos] -ne '"') { throw "JSON: expected a key at offset $($script:JsonPos)" }
            $k = Read-WtlJsonString
            Skip-WtlJsonSpace
            if ([string]$t[$script:JsonPos] -ne ':') { throw "JSON: expected : at offset $($script:JsonPos)" }
            $script:JsonPos++
            $h[$k] = Read-WtlJsonValue
            Skip-WtlJsonSpace
            $d = [string]$t[$script:JsonPos]
            $script:JsonPos++
            if ($d -eq '}') { return $h }
            if ($d -ne ',') { throw "JSON: expected , or } at offset $($script:JsonPos - 1)" }
        }
    }
    if ($c -eq '[') {
        $list = New-Object System.Collections.ArrayList
        $script:JsonPos++
        Skip-WtlJsonSpace
        if ([string]$t[$script:JsonPos] -eq ']') { $script:JsonPos++; return ,($list.ToArray()) }
        while ($true) {
            $item = Read-WtlJsonValue
            [void]$list.Add($item)
            Skip-WtlJsonSpace
            $d = [string]$t[$script:JsonPos]
            $script:JsonPos++
            if ($d -eq ']') { return ,($list.ToArray()) }
            if ($d -ne ',') { throw "JSON: expected , or ] at offset $($script:JsonPos - 1)" }
        }
    }
    if ($c -eq '"') { return (Read-WtlJsonString) }
    foreach ($w in @(@('true', $true), @('false', $false), @('null', $null))) {
        $word = [string]$w[0]
        if ($t.Length -ge $script:JsonPos + $word.Length -and $t.Substring($script:JsonPos, $word.Length) -eq $word) {
            $script:JsonPos += $word.Length
            return $w[1]
        }
    }
    $m = [regex]::Match($t.Substring($script:JsonPos), '^-?[0-9]+(\.[0-9]+)?([eE][+-]?[0-9]+)?')
    if (-not $m.Success) { throw "JSON: unexpected character '$c' at offset $($script:JsonPos)" }
    $script:JsonPos += $m.Length
    if ($m.Groups[1].Success -or $m.Groups[2].Success) { return [double]::Parse($m.Value, [Globalization.CultureInfo]::InvariantCulture) }
    $n = [long]$m.Value
    if ($n -ge [int]::MinValue -and $n -le [int]::MaxValue) { return [int]$n }
    return $n
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
    # with EAP=Stop, stderr output of a native exe redirected via 2>&1 becomes an error record,
    # so run native tools with a local EAP of Continue
    $ErrorActionPreference = 'Continue'
    $out = & $Exe @Arguments 2>&1 | Out-String
    $code = $LASTEXITCODE
    if ($code -ne 0 -and -not $Quiet) { Write-Log ("$Exe exit=$code " + $out.Trim()) }
    return ($code -eq 0)
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

# Winlogon takes the autologon password from the LSA secret "DefaultPassword" before the
# registry value: store it there too, so no older secret wins.
$LsaSource = @'
using System;
using System.Runtime.InteropServices;
public static class WtlLsa {
    [StructLayout(LayoutKind.Sequential)]
    struct LSA_UNICODE_STRING { public ushort Length; public ushort MaximumLength; public IntPtr Buffer; }
    [StructLayout(LayoutKind.Sequential)]
    struct LSA_OBJECT_ATTRIBUTES { public int Length; public IntPtr RootDirectory; public IntPtr ObjectName; public uint Attributes; public IntPtr SecurityDescriptor; public IntPtr SecurityQualityOfService; }
    [DllImport("advapi32.dll")] static extern uint LsaOpenPolicy(IntPtr systemName, ref LSA_OBJECT_ATTRIBUTES attrs, uint access, out IntPtr handle);
    [DllImport("advapi32.dll")] static extern uint LsaStorePrivateData(IntPtr handle, ref LSA_UNICODE_STRING key, ref LSA_UNICODE_STRING data);
    [DllImport("advapi32.dll")] static extern uint LsaClose(IntPtr handle);
    [DllImport("advapi32.dll")] static extern uint LsaNtStatusToWinError(uint status);
    static LSA_UNICODE_STRING Str(string s) {
        LSA_UNICODE_STRING u = new LSA_UNICODE_STRING();
        u.Buffer = Marshal.StringToHGlobalUni(s);
        u.Length = (ushort)(s.Length * 2);
        u.MaximumLength = (ushort)(s.Length * 2 + 2);
        return u;
    }
    public static uint Store(string key, string value) {
        LSA_OBJECT_ATTRIBUTES oa = new LSA_OBJECT_ATTRIBUTES();
        oa.Length = Marshal.SizeOf(typeof(LSA_OBJECT_ATTRIBUTES));
        IntPtr h;
        uint st = LsaOpenPolicy(IntPtr.Zero, ref oa, 0x000F0FFF, out h);
        if (st != 0) return LsaNtStatusToWinError(st);
        LSA_UNICODE_STRING k = Str(key), v = Str(value);
        try { st = LsaStorePrivateData(h, ref k, ref v); }
        finally { Marshal.FreeHGlobal(k.Buffer); Marshal.FreeHGlobal(v.Buffer); LsaClose(h); }
        return LsaNtStatusToWinError(st);
    }
}
'@

# ---------------------------------------------------------------- stage files

function Initialize-WtlDir {
    if (-not (Test-Path $WtlDir)) { New-Item -ItemType Directory -Path $WtlDir -Force | Out-Null }
    $src = $null
    if ($ScriptDir -and ((Resolve-Path $ScriptDir).Path.TrimEnd('\') -ne $WtlDir)) {
        $src = $ScriptDir
    } else {
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

# ---------------------------------------------------------------- the machine

# The image's Administrator password (the VHD's own) becomes the build's, and the machine logs
# on as Administrator from now on: the 0-state build starts from that autologon.
function Set-AdminLogon([string]$Password) {
    if (-not (Invoke-Native 'net.exe' @('user', 'Administrator', $Password))) {
        throw 'setting the Administrator password failed (net user)'
    }
    $winlogon = 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon'
    Set-RegValue $winlogon 'AutoAdminLogon' '1' 'String'
    Set-RegValue $winlogon 'DefaultUserName' 'Administrator' 'String'
    Set-RegValue $winlogon 'DefaultPassword' $Password 'String'
    Set-RegValue $winlogon 'DefaultDomainName' $env:COMPUTERNAME 'String'
    Remove-RegValue $winlogon 'AutoLogonCount'
    if (-not ('WtlLsa' -as [type])) { Add-Type -TypeDefinition $LsaSource }
    $rc = [WtlLsa]::Store('DefaultPassword', $Password)
    if ($rc -ne 0) { throw "storing the autologon password in the LSA failed (error $rc)" }
}

# NB: the build does NOT rename the computer. The lab renames it to its machine name later,
# and renaming an imported image here only leaves a pending-reboot state ("You must restart to
# apply these changes") that disrupts the first lab boot and makes the autologon DefaultDomainName
# stale. The base image keeps whatever name it had; the lab sets the real one.

function Set-SystemConfig {
    Invoke-Step 'firewall' {
        # 2008 R2 has the advanced firewall; 2003 only the older "netsh firewall"
        if (-not (Invoke-Native 'netsh.exe' @('advfirewall', 'set', 'allprofiles', 'state', 'off') -Quiet)) {
            [void](Invoke-Native 'netsh.exe' @('firewall', 'set', 'opmode', 'disable') -Quiet)
        }
    }
    Invoke-Step 'server manager' {
        Set-RegValue 'HKLM:\SOFTWARE\Microsoft\ServerManager' 'DoNotOpenServerManagerAtLogon' 1
        Set-RegValue 'HKLM:\SOFTWARE\Microsoft\ServerManager\oobe' 'DoNotOpenInitialConfigurationTasksAtLogon' 1
        Set-RegValue 'HKCU:\Software\Microsoft\ServerManager' 'DoNotOpenServerManagerAtLogon' 1
    }
    Invoke-Step 'IE ESC off' {
        foreach ($id in @('{A509B1A7-37EF-4b3f-8CFC-4F3A74704073}', '{A509B1A8-37EF-4b3f-8CFC-4F3A74704073}')) {
            $k = "HKLM:\SOFTWARE\Microsoft\Active Setup\Installed Components\$id"
            if (Test-Path $k) { Set-RegValue $k 'IsInstalled' 0 }
        }
    }
    Invoke-Step 'power' {
        [void](Invoke-Native 'powercfg.exe' @('-setactive', '8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c'))
        foreach ($t in @('monitor-timeout-ac', 'monitor-timeout-dc', 'standby-timeout-ac', 'standby-timeout-dc', 'disk-timeout-ac', 'disk-timeout-dc', 'hibernate-timeout-ac', 'hibernate-timeout-dc')) {
            [void](Invoke-Native 'powercfg.exe' @('-change', "-$t", '0') -Quiet)
        }
        [void](Invoke-Native 'powercfg.exe' @('-h', 'off'))
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
}

# ---------------------------------------------------------------- main

function Invoke-Main {
    Write-Log "setup.ps1 (2008) start from $ScriptDir"
    Initialize-WtlDir

    $mutex = New-Object System.Threading.Mutex($false, 'Global\WTL-Build')
    $owned = $false
    try { $owned = $mutex.WaitOne(0) } catch [System.Threading.AbandonedMutexException] { $owned = $true }
    if (-not $owned) { Write-Log 'setup.ps1 already running, exiting'; exit 0 }
    $script:Mutex = $mutex

    $cfg = ConvertFrom-WtlJson ([System.IO.File]::ReadAllText((Join-Path $WtlDir 'build.json')))
    $password = [string]$cfg.password
    if (-not $password) { $password = $DefaultPassword }

    $osCaption = 'unknown'
    try { $osCaption = ([string](Get-WmiObject -Class Win32_OperatingSystem).Caption).Trim() } catch { }
    Write-Log "os=$($cfg.os) caption=$osCaption ps=$($PSVersionTable.PSVersion) user=$env:USERDOMAIN\$env:USERNAME"

    Set-AdminLogon $password
    Write-Log 'Administrator password + autologon set'
    Set-SystemConfig
    Write-Log 'system configuration applied'

    # The agent autostart goes in LAST, after BUILD_DONE (as in guest/setup.ps1): an agent
    # started earlier would hold COM1 and the host would never see BUILD_DONE.
    Send-WtlEvent 'BUILD_DONE'
    # Launch the agent through cmd.exe, NOT bare "powershell.exe ... -File ..." and NOT with
    # "-WindowStyle Hidden": on PowerShell 2.0 (2008 R2 / 2003), an HKLM Run entry that invokes
    # powershell.exe directly silently never runs the script (and -WindowStyle Hidden breaks its
    # argument parsing outright). The cmd.exe wrapper runs it reliably. The shared PS 4+
    # guest/setup.ps1 uses the bare hidden form, which is fine there.
    Set-RegValue $RunKey 'WTL-Agent' "cmd.exe /c powershell.exe -NoProfile -ExecutionPolicy Bypass -File $WtlDir\agent.ps1" 'String'
    try { [System.IO.File]::AppendAllText($LogFile, (Get-Date).ToString('yyyy-MM-dd HH:mm:ss') + " agent autostart installed (HKLM Run WTL-Agent)`r`n") } catch { }
    Start-Sleep -Seconds 2
    try { Stop-Computer -Force -ErrorAction Stop } catch { [void](Invoke-Native 'shutdown.exe' @('/s', '/t', '0', '/f')) }
}

try {
    Invoke-Main
} catch {
    $msg = $_.Exception.Message
    try { $msg = "$msg (line $($_.InvocationInfo.ScriptLineNumber))" } catch { }
    Write-WtlLine ("WTL BUILD_FAIL | " + $msg)
    # Do NOT shut down: leave the VM up so the host can take a screenshot.
    while ($true) { Start-Sleep -Seconds 3600 }
}
