# lab.ps1 (2008) - win-test-lab lab, guest side, for Windows Server 2008 R2.
#
# PowerShell 2.0 / .NET 2.0 compatible: what Server 2008 R2 ships (tests/ps_lint.py --ps2).
# Dot-sourced by agent.ps1 when a CD has \LAB\lab.ps1 + \LAB\lab.json (the control disc the
# host's `wtl` writes); uses the agent's Send-WtlEvent, Send-Output, Stop-Tree and
# Get-ExitCodeSafe. Runs at every logon and continues where it left off.
#
# The standalone (workgroup) flow of guest/lab.ps1: no domain, no roles, no lab LAN (the host
# refuses those for 2008 before anything boots):
#
#   phase new:     the SYSTEM worker, the Administrator's lab password, the lab's local users
#                  and groups, the computer renamed to its machine key, autologon as the logon
#                  user -> WTL LAB_JOINED, reboot
#   phase joined:  logged on as that user -> WTL LAB_CLIENT_READY [-> the run's steps]
#
# Invoke-Lab LABDIR -> 'ready' | 'done' (steps ran) | 'reboot' | 'fail'
# State: C:\WTL\lab-state.json {"phase": "new|joined"}.
# Every command's output is swallowed: a PowerShell function returns everything it emits.

$LabStateFile = 'C:\WTL\lab-state.json'
$LabWinlogon  = 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon'

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

# A small JSON writer for the worker's requests: hashtables, arrays, strings, numbers, bools.
function ConvertTo-WtlJsonString([string]$S) {
    $sb = New-Object System.Text.StringBuilder
    [void]$sb.Append('"')
    foreach ($ch in $S.ToCharArray()) {
        if ($ch -eq '"') { [void]$sb.Append('\"') }
        elseif ($ch -eq '\') { [void]$sb.Append('\\') }
        elseif ([int]$ch -lt 0x20) { [void]$sb.Append(('\u{0:x4}' -f [int]$ch)) }
        else { [void]$sb.Append($ch) }
    }
    [void]$sb.Append('"')
    return $sb.ToString()
}

function ConvertTo-WtlJson($Value) {
    if ($null -eq $Value) { return 'null' }
    if ($Value -is [bool]) { if ($Value) { return 'true' } else { return 'false' } }
    if ($Value -is [int] -or $Value -is [long] -or $Value -is [double]) { return [string]::Format([Globalization.CultureInfo]::InvariantCulture, '{0}', $Value) }
    if ($Value -is [System.Collections.IDictionary]) {
        $parts = @()
        foreach ($k in $Value.Keys) { $parts += (ConvertTo-WtlJsonString ([string]$k)) + ':' + (ConvertTo-WtlJson $Value[$k]) }
        return '{' + ($parts -join ',') + '}'
    }
    if ($Value -is [System.Array]) {
        $parts = @()
        foreach ($x in $Value) { $parts += (ConvertTo-WtlJson $x) }
        return '[' + ($parts -join ',') + ']'
    }
    return (ConvertTo-WtlJsonString ([string]$Value))
}

# ---------------------------------------------------------------- state + helpers

function Get-LabPhase {
    if (Test-Path -LiteralPath $LabStateFile) {
        try { return [string](ConvertFrom-WtlJson ([System.IO.File]::ReadAllText($LabStateFile))).phase } catch { }
    }
    return 'new'
}

function Set-LabPhase([string]$Phase) {
    [System.IO.File]::WriteAllText($LabStateFile, ('{"phase": "' + $Phase + '", "at": "' + (Get-Date).ToString('s') + '"}'))
}

function Send-LabPhase([string]$Text) { Send-WtlEvent 'LAB_PHASE' @() $Text }

function Invoke-LabNative([string]$Exe, [string[]]$Arguments) {
    $ErrorActionPreference = 'Continue'
    $out = & $Exe @Arguments 2>&1 | Out-String
    if ($LASTEXITCODE -ne 0) { Send-LabPhase ("$Exe " + ($Arguments -join ' ') + " exit=$LASTEXITCODE " + $out.Trim()) }
}

# Run a native command only for its exit code (its output, stderr included, is dropped). The
# agent runs with ErrorActionPreference=Stop, under which a native command's stderr would throw.
function Test-LabNative([string]$Exe, [string[]]$Arguments) {
    $ErrorActionPreference = 'Continue'
    $null = & $Exe @Arguments 2>&1
    return ($LASTEXITCODE -eq 0)
}

# Winlogon takes the autologon password from the LSA secret "DefaultPassword" before the registry
# value; the base image's setup.ps1 stored the Administrator password there.
$LabLsaSource = @'
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

function Set-LabAutoLogon([string]$Domain, [string]$User, [string]$Password) {
    Set-ItemProperty -Path $LabWinlogon -Name 'AutoAdminLogon' -Value '1'
    Set-ItemProperty -Path $LabWinlogon -Name 'DefaultUserName' -Value $User
    Set-ItemProperty -Path $LabWinlogon -Name 'DefaultDomainName' -Value $Domain
    Set-ItemProperty -Path $LabWinlogon -Name 'DefaultPassword' -Value $Password
    Remove-ItemProperty -Path $LabWinlogon -Name 'AutoLogonCount' -ErrorAction SilentlyContinue
    if (-not ('WtlLsa' -as [type])) { Add-Type -TypeDefinition $LabLsaSource }
    $rc = [WtlLsa]::Store('DefaultPassword', $Password)
    if ($rc -ne 0) { throw "storing the autologon password in the LSA failed (error $rc)" }
}

# lets the host power the VM off (QMP system_powerdown) whoever is logged on, without waiting on apps
function Set-LabShutdownFriendly {
    $sys = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System'
    if (-not (Test-Path $sys)) { New-Item -Path $sys -Force | Out-Null }
    New-ItemProperty -Path $sys -Name 'ShutdownWithoutLogon' -Value 1 -PropertyType DWord -Force | Out-Null
    New-ItemProperty -Path 'HKCU:\Control Panel\Desktop' -Name 'AutoEndTasks' -Value '1' -PropertyType String -Force | Out-Null
    # new profiles (the lab's users) start from the Default user hive. 2008 R2:
    # C:\Users\Default; 2003: "C:\Documents and Settings\Default User".
    $hive = $null
    foreach ($p in @('C:\Users\Default\NTUSER.DAT', 'C:\Documents and Settings\Default User\NTUSER.DAT')) {
        if (Test-Path -LiteralPath $p) { $hive = $p; break }
    }
    if ($hive) {
        Invoke-LabNative 'reg.exe' @('load', 'HKU\WTLDefault', $hive)
        Invoke-LabNative 'reg.exe' @('add', 'HKU\WTLDefault\Control Panel\Desktop', '/v', 'AutoEndTasks', '/t', 'REG_SZ', '/d', '1', '/f')
        [GC]::Collect()
        Invoke-LabNative 'reg.exe' @('unload', 'HKU\WTLDefault')
    }
}

# Grant the built-in Users group Modify on a folder (recursively), via .NET ACLs so it works the
# same on Server 2003 and 2008 R2 (icacls is Vista+; cacls has an awkward prompt). SID S-1-5-32-545.
function Grant-LabUsersModify([string]$Dir) {
    try {
        $sid = New-Object System.Security.Principal.SecurityIdentifier('S-1-5-32-545')
        # PowerShell 2.0's Get-Acl/Set-Acl have -Path, not -LiteralPath
        $acl = Get-Acl -Path $Dir
        $rule = New-Object System.Security.AccessControl.FileSystemAccessRule(
            $sid, 'Modify', 'ContainerInherit,ObjectInherit', 'None', 'Allow')
        $acl.AddAccessRule($rule)
        Set-Acl -Path $Dir -AclObject $acl
    } catch { Send-LabPhase "granting Users modify on ${Dir}: $($_.Exception.Message)" }
}

# The local Administrator gets the lab's admin_password (the base image has the default one),
# and the next autologon uses it.
function Set-LabLocalAdmin($Cfg) {
    $pw = [string]$Cfg.admin_password
    if (-not $pw) { return }
    Invoke-LabNative 'net.exe' @('user', 'Administrator', $pw)
    Set-LabAutoLogon $env:COMPUTERNAME 'Administrator' $pw
}

# The lab's users as local accounts on this machine, in their (local) groups; a group that
# does not exist is created. Passwords never expire.
function New-LabLocalUsers($Cfg) {
    foreach ($u in @($Cfg.users)) {
        if (-not $u) { continue }
        $name = [string]$u.name
        $pw = [string]$u.password
        if (Test-LabNative 'net.exe' @('user', $name)) { Invoke-LabNative 'net.exe' @('user', $name, $pw) }
        else { Invoke-LabNative 'net.exe' @('user', $name, $pw, '/add') }
        if (-not (Test-LabNative 'net.exe' @('user', $name))) { throw "creating the local user $name failed (password too simple, or contains the user name?)" }
        try {
            $acct = [ADSI]"WinNT://./$name,user"
            $acct.UserFlags.Value = $acct.UserFlags.Value -bor 0x10000   # DONT_EXPIRE_PASSWD
            $acct.SetInfo()
        } catch { Send-LabPhase "user ${name}: could not set 'password never expires': $($_.Exception.Message)" }
        $groups = @($u.groups | Where-Object { $_ })
        foreach ($g in $groups) {
            if (-not (Test-LabNative 'net.exe' @('localgroup', $g))) { Invoke-LabNative 'net.exe' @('localgroup', $g, '/add') }
            [void](Test-LabNative 'net.exe' @('localgroup', $g, $name, '/add'))   # fails if already a member
            if (-not (Test-LabNative 'net.exe' @('localgroup', $g))) { throw "creating the local group $g failed" }
        }
        $in = ''
        if ($groups.Count) { $in = ' in ' + ($groups -join ', ') }
        Send-LabPhase "local user $name$in"
    }
}

# Rename-Computer is PowerShell 3.0+: WMI. Takes effect at the next boot.
function Rename-LabComputer([string]$Want) {
    if (-not $Want -or $env:COMPUTERNAME -eq $Want) { return }
    Send-LabPhase "renaming this computer $env:COMPUTERNAME -> $Want"
    $r = (Get-WmiObject -Class Win32_ComputerSystem).Rename($Want)
    if ($r.ReturnValue -ne 0) { throw "renaming the computer to $Want failed (Win32_ComputerSystem.Rename returned $($r.ReturnValue))" }
}

# Register the SYSTEM worker (labsvc.ps1) to run at every boot, while we are still
# Administrator; the lab's logon user later talks to it via C:\WTL\svc. labsvc.ps1 and
# agent.ps1 ship on the control disc next to lab.ps1: copy them to the fixed baked paths first.
function Register-LabSvc([string]$LabDir) {
    foreach ($f in 'labsvc.ps1', 'agent.ps1') {
        $src = Join-Path $LabDir $f
        if (-not (Test-Path -LiteralPath $src)) { continue }
        $dst = Join-Path 'C:\WTL' $f
        Copy-Item -LiteralPath $src -Destination $dst -Force
        # files from a CD keep ReadOnly, which would block the next update
        try { (Get-Item -LiteralPath $dst).IsReadOnly = $false } catch { }
    }
    # cmd.exe wrapper, no -WindowStyle Hidden: PowerShell 2.0 does not launch reliably otherwise
    # (see the WTL-Agent note in the 2008/2003 setup.ps1)
    $cmd = 'cmd.exe /c powershell.exe -NoProfile -ExecutionPolicy Bypass -File C:\WTL\labsvc.ps1'
    Invoke-LabNative 'schtasks.exe' @('/Create', '/TN', 'WTL-Svc', '/TR', $cmd, '/SC', 'ONSTART',
        '/RU', 'SYSTEM', '/RL', 'HIGHEST', '/F')
    Invoke-LabNative 'schtasks.exe' @('/Run', '/TN', 'WTL-Svc')
}

# ---------------------------------------------------------------- the machine

function Invoke-LabClient($Cfg, [string]$LabDir) {
    if ([string]$Cfg.mode -ne 'workgroup') { throw 'Windows Server 2008 R2 machines run standalone (workgroup) labs only; this lab is an AD lab' }
    if ($Cfg.lan) { throw 'Windows Server 2008 R2 machines run without a lab LAN (one machine per lab)' }
    $phase = Get-LabPhase
    # the account this machine logs on as: its logon user, else Administrator, on this machine
    $user = [string]$Cfg.logon_user
    $pass = [string]$Cfg.logon_password
    if (-not $user) { $user = 'Administrator'; $pass = [string]$Cfg.admin_password }
    $want = [string]$Cfg.machine
    if ($phase -eq 'new') {
        foreach ($k in @('roles', 'features', 'setup')) {
            if (@($Cfg.$k | Where-Object { $_ }).Count -gt 0) { throw "Windows Server 2008 R2 machines have no '$k' yet" }
        }
        Set-LabShutdownFriendly
        # the agent runs as the logon user from now on: it writes its log and output under C:\WTL
        Grant-LabUsersModify 'C:\WTL'
        Register-LabSvc $LabDir
        Set-LabLocalAdmin $Cfg
        New-LabLocalUsers $Cfg
        Rename-LabComputer $want
        Set-LabAutoLogon $want $user $pass
        Set-LabPhase 'joined'
        Send-WtlEvent 'LAB_JOINED' @('workgroup', $want, 'autologon', "$want\$user")
        Restart-Computer -Force
        return 'reboot'
    }

    $who = "$env:USERDOMAIN\$env:USERNAME"
    if ($env:USERDOMAIN -ne $want -or $env:USERNAME -ne $user) {
        throw "logged on as $who, expected $want\$user (autologon)"
    }
    $cs = Get-WmiObject -Class Win32_ComputerSystem
    Send-WtlEvent 'LAB_CLIENT_READY' @('user', $who, 'workgroup', $cs.Workgroup, 'computer', $env:COMPUTERNAME, 'ip', $Cfg.ip)
    return 'ready'
}

# ---------------------------------------------------------------- the SYSTEM worker (labsvc.ps1)
$SvcDir = 'C:\WTL\svc'

# lab.json "slow": how many times longer every fixed wait here is (>1 when the host emulates this
# machine's CPU). Set by Invoke-Lab.
$script:LabSlow = 1

# The worker is a boot task (WTL-Svc), and Windows starts boot tasks when it gets to them. Until
# the worker has answered once in this boot, a request may wait this long for it to start;
# after that, the normal timeout.
$SvcStartS = 900
$script:SvcAnswered = $false

function Invoke-SvcRequest($Req, [int]$TimeoutS = 120) {
    $TimeoutS = $TimeoutS * $script:LabSlow
    if (-not $script:SvcAnswered) { $TimeoutS = [Math]::Max($TimeoutS, $SvcStartS * $script:LabSlow) }
    if (-not (Test-Path $SvcDir)) { New-Item -ItemType Directory -Path $SvcDir -Force | Out-Null }
    $id = [Guid]::NewGuid().ToString('N')
    $reqPath = Join-Path $SvcDir ($id + '.req.json')
    $resPath = Join-Path $SvcDir ($id + '.res.json')
    # written under another name and renamed: the worker never reads half a request
    [System.IO.File]::WriteAllText($reqPath + '.tmp', (ConvertTo-WtlJson $Req))
    [System.IO.File]::Move($reqPath + '.tmp', $reqPath)
    $start = [DateTime]::Now
    $deadline = $start.AddSeconds($TimeoutS)
    $told = $false
    while (-not (Test-Path -LiteralPath $resPath)) {
        if ([DateTime]::Now -ge $deadline) { throw "the privileged worker did not answer within ${TimeoutS}s (is WTL-Svc running?)" }
        if (-not $script:SvcAnswered -and -not $told -and ([DateTime]::Now - $start).TotalSeconds -ge 30) {
            Send-LabPhase 'waiting for the SYSTEM worker (WTL-Svc) to start'
            $told = $true
        }
        Start-Sleep -Milliseconds 300
    }
    Start-Sleep -Milliseconds 100
    $res = ConvertFrom-WtlJson ([System.IO.File]::ReadAllText($resPath))
    try { Remove-Item -LiteralPath $resPath -Force } catch { }
    if (-not $script:SvcAnswered -and $told) {
        Send-LabPhase ('the SYSTEM worker answered after {0:N0}s' -f ([DateTime]::Now - $start).TotalSeconds)
    }
    $script:SvcAnswered = $true
    return $res
}

# Assign each mounted disc its fixed drive letter (needs privilege -> the SYSTEM worker).
function Set-LabMounts($Mounts) {
    foreach ($m in @($Mounts)) {
        if (-not $m) { continue }
        $letter = ([string]$m.letter).ToUpper()
        $res = Invoke-SvcRequest @{ action = 'mount'; letter = $letter }
        if (-not $res.ok) { throw "mount ${letter}: failed: $($res.error)" }
        Send-WtlEvent 'LAB_MOUNT' @('letter', $res.letter, 'host', [string]$m.host)
    }
}

# ---------------------------------------------------------------- run steps
#
# A step's `as` is "system" (the SYSTEM worker), "logon" (this machine's own logged-on user,
# foreground), or "user" (another local lab user, started by the worker with its password).
# Each LAB_STEP carries machine= so the host can tell the machines' reports apart.

function Invoke-LabMemberSteps($Cfg) {
    $run = $Cfg.run
    $machine = [string]$Cfg.machine
    if ($run -eq $null) { Send-WtlEvent 'LAB_RUN_DONE' @('machine', $machine, 'steps', 0) 'no run steps'; return 'done' }
    $steps = @($run.steps | Where-Object { $_ })
    $defWait = 60; if ($run.wait) { try { $defWait = [int]$run.wait } catch { } }

    Set-LabMounts $run.mounts

    $n = 0
    foreach ($s in $steps) {
        $n++
        $stepWait = $defWait; if ($s.wait) { try { $stepWait = [int]$s.wait } catch { } }
        [void](Invoke-LabMultiStep $Cfg $machine $n $s $stepWait)
    }
    Send-WtlEvent 'LAB_RUN_DONE' @('machine', $machine, 'steps', $n)
    return 'done'
}

# Run one step on this machine. $S has: as_ (system|logon|user), user, password, path, args,
# keep_running. Reports LAB_STEP machine=.. as=.. n=.. script=.. exit=.. [secs=..].
function Invoke-LabMultiStep($Cfg, [string]$Machine, [int]$N, $S, [int]$TimeoutS) {
    $as = [string]$S.as_
    $path = [string]$S.path
    $argsLine = [string]$S.args
    $keep = $false; if ($S.keep_running) { $keep = [bool]$S.keep_running }
    $runDir = 'C:\WTL\run'
    if (-not (Test-Path $runDir)) { New-Item -ItemType Directory -Path $runDir -Force | Out-Null }
    $out = Join-Path $runDir ("step$N.out"); $err = Join-Path $runDir ("step$N.err")
    $cwd = [System.IO.Path]::GetDirectoryName($path)
    if (-not $cwd) { $cwd = ($path.Substring(0, 2) + '\') }

    $req = @{ action = 'run'; path = $path; cwd = $cwd; timeout = $TimeoutS; out = $out; err = $err;
              args = $argsLine; keep_running = $keep }
    if ($as -eq 'user') {
        $req['user'] = [string]$S.user
        $req['password'] = [string]$S.password
        $req['domain'] = '.'   # a local account
    }

    if ($as -eq 'logon') {
        # run in this machine's own interactive session (the logged-on user)
        $res = Invoke-LabStepForeground $N $path $argsLine $cwd $out $err $TimeoutS $keep
    } else {
        # 'system' or 'user': the SYSTEM worker runs it
        $res = Invoke-SvcRequest $req ($TimeoutS + 60)
    }
    if (-not $res.ok) {
        Send-WtlEvent 'LAB_STEP' @('machine', $Machine, 'as', $as, 'n', $N, 'script', $path, 'exit', 'launchfail') ([string]$res.error)
        return 1
    }
    Send-Output ("step" + $N + "out") $out
    Send-Output ("step" + $N + "err") $err
    if ($res.running) {
        Send-WtlEvent 'LAB_STEP' @('machine', $Machine, 'as', $as, 'n', $N, 'script', $path, 'exit', 'running', 'secs', $res.secs)
        return 0
    }
    if ($res.timed_out) {
        Send-WtlEvent 'LAB_STEP' @('machine', $Machine, 'as', $as, 'n', $N, 'script', $path, 'exit', 'timeout', 'secs', $res.secs)
        return 2
    }
    Send-WtlEvent 'LAB_STEP' @('machine', $Machine, 'as', $as, 'n', $N, 'script', $path, 'exit', ([int]$res.exit), 'secs', $res.secs)
    return 0
}

# Run a step in the current (logged-on) session, capturing its stdout/stderr to files. With
# $Keep, a program still running at the deadline is left running and counts as "running".
function Invoke-LabStepForeground([int]$N, [string]$Path, [string]$ArgsLine, [string]$Cwd, [string]$Out, [string]$Err, [int]$TimeoutS, [bool]$Keep) {
    $runDir = 'C:\WTL\run'
    $inF = Join-Path $runDir 'empty.in'
    foreach ($f in @($Out, $Err)) { if (Test-Path $f) { Remove-Item $f -Force } }
    [System.IO.File]::WriteAllText($inF, '')
    $ext = [System.IO.Path]::GetExtension($Path).ToLowerInvariant()
    $file = $Path; $argLine = ''
    if ($ext -eq '.ps1') {
        $file = 'powershell.exe'; $argLine = '-NoProfile -ExecutionPolicy Bypass -File "' + $Path + '"'
        if ($ArgsLine) { $argLine += ' ' + $ArgsLine }
    } elseif ($ext -eq '.bat' -or $ext -eq '.cmd') {
        $file = $env:ComSpec; $argLine = '/d /c ""' + $Path + '"'
        if ($ArgsLine) { $argLine += ' ' + $ArgsLine }
        $argLine += '"'
    } elseif ($ext -eq '.msi') {
        $file = 'msiexec.exe'; $argLine = '/i "' + $Path + '" /qn /norestart'
        if ($ArgsLine) { $argLine += ' ' + $ArgsLine }
    } elseif ($ArgsLine) {
        $argLine = $ArgsLine
    }
    $sp = @{ FilePath = $file; PassThru = $true; NoNewWindow = $true;
             RedirectStandardOutput = $Out; RedirectStandardError = $Err; RedirectStandardInput = $inF }
    if (Test-Path -LiteralPath $Cwd) { $sp['WorkingDirectory'] = $Cwd }
    if ($argLine) { $sp['ArgumentList'] = $argLine }
    $t0 = [DateTime]::Now
    $proc = $null
    try { $proc = Start-Process @sp } catch { return @{ ok = $false; error = $_.Exception.Message } }
    try { $null = $proc.Handle } catch { }
    $deadline = $t0.AddSeconds([Math]::Max(5, $TimeoutS))
    $timedOut = $false
    while (-not $proc.HasExited) {
        if ([DateTime]::Now -ge $deadline) { $timedOut = $true; break }
        Start-Sleep -Milliseconds 250
    }
    $secs = [Math]::Round(([DateTime]::Now - $t0).TotalSeconds, 1)
    if ($timedOut -and $Keep) { return @{ ok = $true; running = $true; secs = $secs } }
    if ($timedOut) { Stop-Tree $proc; Start-Sleep -Seconds 1; return @{ ok = $true; timed_out = $true; secs = $secs } }
    return @{ ok = $true; exit = (Get-ExitCodeSafe $proc); secs = $secs }
}

function Invoke-LabMulti($Cfg, [string]$LabDir) {
    if ([string]$Cfg.role -ne 'member') { throw "Windows Server 2008 R2 machines cannot have the role '$($Cfg.role)' yet" }
    $r = Invoke-LabClient $Cfg $LabDir
    if ($r -ne 'ready') { return $r }
    # the 0-state build: the finished machine is frozen; the host powers it off via ACPI
    if ($Cfg.shutdown_when_ready) { Send-WtlEvent 'LAB_IDLE'; return 'reboot' }
    if ($Cfg.run -ne $null) { return (Invoke-LabMemberSteps $Cfg) }
    return 'ready'
}

# ================================================================ entry

function Invoke-Lab([string]$LabDir) {
    try {
        $cfg = ConvertFrom-WtlJson ([System.IO.File]::ReadAllText((Join-Path $LabDir 'lab.json')))
        Send-WtlEvent 'LAB_START' @('role', $cfg.role, 'phase', (Get-LabPhase), 'user', "$env:USERDOMAIN\$env:USERNAME")
        if ([string]$cfg.fmt -ne 'multi') { throw "lab.json fmt '$($cfg.fmt)' is not supported (this lab.ps1 reads fmt=multi; rebuild with the current lab)" }
        if ($cfg.slow) { try { $script:LabSlow = [Math]::Max(1, [int]$cfg.slow) } catch { } }
        return (Invoke-LabMulti $cfg $LabDir)
    } catch {
        $msg = $_.Exception.Message
        try { $msg = "$msg (line $($_.InvocationInfo.ScriptLineNumber))" } catch { }
        Send-WtlEvent 'LAB_FAIL' @() $msg
        return 'fail'
    }
}
