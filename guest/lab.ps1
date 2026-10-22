# lab.ps1 - win-test-lab AD lab, guest side.
#
# PowerShell 4.0 compatible (Server 2012 R2 baseline). No PS5+ features.
# Dot-sourced by agent.ps1 when a CD has \LAB\lab.ps1 (+ \LAB\lab.json, written by lab.sh);
# uses the agent's Send-WtlEvent. Runs at every logon and continues where it left off:
#
#   role dc:      static IP on the lab NIC -> AD DS role + new forest -> reboot
#                 -> (LAB\Administrator) users -> WTL LAB_DC_READY
#   role client:  static IP, DNS = DC -> wait for the DC -> join the domain, autologon = the
#                 domain user -> reboot -> (logged on as e.g. LAB\student) WTL LAB_CLIENT_READY
#
# Invoke-Lab LABDIR -> 'ready' (agent continues with the job) | 'reboot' | 'fail'
# State: C:\WTL\lab-state.json {"phase": "new|promoted|ready|joined"}.
# Every command's output is swallowed: a PowerShell function returns everything it emits.

$LabStateFile = 'C:\WTL\lab-state.json'
$LabWinlogon  = 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon'

function Get-LabPhase {
    if (Test-Path -LiteralPath $LabStateFile) {
        try { return [string](([System.IO.File]::ReadAllText($LabStateFile) | ConvertFrom-Json).phase) } catch { }
    }
    return 'new'
}

function Set-LabPhase([string]$Phase) {
    [System.IO.File]::WriteAllText($LabStateFile, ('{"phase": "' + $Phase + '", "at": "' + (Get-Date).ToString('s') + '"}'))
}

function Send-LabPhase([string]$Text) { Send-WtlEvent 'LAB_PHASE' @() $Text }

# multi-machine labs only: rename this Windows box to its lab machine key ($Cfg.machine) and reboot.
# Returns $true if a rename+reboot was started (caller should stop and let the reboot happen), $false
# if the name already matches or no machine key is set (the two-machine .conf path leaves it unset).
function Rename-LabComputer($Cfg) {
    $want = [string]$Cfg.machine
    if (-not $want -or $env:COMPUTERNAME -eq $want) { return $false }
    Set-LabShutdownFriendly
    Send-LabPhase "renaming this computer $env:COMPUTERNAME -> $want, then reboot"
    Rename-Computer -NewName $want -Force -ErrorAction Stop
    Send-WtlEvent 'LAB_REBOOT' @('phase', 'rename')
    Restart-Computer -Force
    return $true
}

function ConvertTo-LabSecure([string]$Text) { return (ConvertTo-SecureString $Text -AsPlainText -Force) }

function Invoke-LabNative([string]$Exe, [string[]]$Arguments) {
    $ErrorActionPreference = 'Continue'
    $out = & $Exe @Arguments 2>&1 | Out-String
    if ($LASTEXITCODE -ne 0) { Send-LabPhase ("$Exe " + ($Arguments -join ' ') + " exit=$LASTEXITCODE " + $out.Trim()) }
}

# Winlogon takes the autologon password from the LSA secret "DefaultPassword" before the registry
# value; Windows Setup's <AutoLogon> stored the Administrator password there.
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
    # new profiles (the domain user) start from the Default user hive
    Invoke-LabNative 'reg.exe' @('load', 'HKU\WTLDefault', 'C:\Users\Default\NTUSER.DAT')
    Invoke-LabNative 'reg.exe' @('add', 'HKU\WTLDefault\Control Panel\Desktop', '/v', 'AutoEndTasks', '/t', 'REG_SZ', '/d', '1', '/f')
    [GC]::Collect()
    Invoke-LabNative 'reg.exe' @('unload', 'HKU\WTLDefault')
}

function Get-LabAdapter([string]$Mac) {
    $want = $Mac.ToUpper().Replace(':', '-')
    $deadline = (Get-Date).AddMinutes(3)
    while ($true) {
        $a = Get-NetAdapter -ErrorAction SilentlyContinue | Where-Object { $_.MacAddress -eq $want } | Select-Object -First 1
        if ($a) { return $a }
        if ((Get-Date) -ge $deadline) { throw "no network adapter with MAC $Mac" }
        Start-Sleep -Seconds 3
    }
}

# static IP on the lab NIC; every adapter uses the DC for DNS and registers nothing but the lab IP
function Set-LabNetwork($Cfg) {
    $a = Get-LabAdapter ([string]$Cfg.lan_mac)
    $if = $a.ifIndex
    try { Rename-NetAdapter -Name $a.Name -NewName 'LAB' -ErrorAction Stop } catch { }
    $have = @(Get-NetIPAddress -InterfaceIndex $if -AddressFamily IPv4 -ErrorAction SilentlyContinue | Where-Object { $_.IPAddress -eq $Cfg.ip })
    if ($have.Count -eq 0) {
        Set-NetIPInterface -InterfaceIndex $if -Dhcp Disabled -ErrorAction SilentlyContinue
        Get-NetIPAddress -InterfaceIndex $if -AddressFamily IPv4 -ErrorAction SilentlyContinue | Remove-NetIPAddress -Confirm:$false -ErrorAction SilentlyContinue
        New-NetIPAddress -InterfaceIndex $if -IPAddress ([string]$Cfg.ip) -PrefixLength ([int]$Cfg.prefix) | Out-Null
    }
    Set-NetIPInterface -InterfaceIndex $if -InterfaceMetric 5 -ErrorAction SilentlyContinue
    foreach ($n in @(Get-NetAdapter -ErrorAction SilentlyContinue)) {
        Set-DnsClientServerAddress -InterfaceIndex $n.ifIndex -ServerAddresses @([string]$Cfg.dc_ip)
        if ($n.ifIndex -ne $if) { Set-DnsClient -InterfaceIndex $n.ifIndex -RegisterThisConnectionsAddress $false -ErrorAction SilentlyContinue }
    }
    Send-LabPhase "network: $($Cfg.ip)/$($Cfg.prefix) on the lab NIC, DNS $($Cfg.dc_ip)"
}

# ---------------------------------------------------------------- domain controller

function New-LabUser($Cfg, $U) {
    $name = [string]$U.name
    $pw = ConvertTo-LabSecure ([string]$U.password)
    $exists = Get-ADUser -Filter "SamAccountName -eq '$name'" -ErrorAction SilentlyContinue
    if (-not $exists) {
        New-ADUser -Name $name -SamAccountName $name -UserPrincipalName "$name@$($Cfg.domain)" `
            -AccountPassword $pw -Enabled $true -PasswordNeverExpires $true -ChangePasswordAtLogon $false
    } else {
        # left over from an earlier run (e.g. created disabled when its password was rejected)
        Set-ADAccountPassword -Identity $name -Reset -NewPassword $pw
        Set-ADUser -Identity $name -PasswordNeverExpires $true -ChangePasswordAtLogon $false
        Enable-ADAccount -Identity $name
    }
    $groups = @($U.groups | Where-Object { $_ })
    foreach ($g in $groups) {
        # a group named in the lab but not in AD yet is created (a global security group under Users)
        $have = Get-ADGroup -Filter "Name -eq '$g'" -ErrorAction SilentlyContinue
        if (-not $have) { New-ADGroup -Name $g -GroupScope Global -GroupCategory Security }
        Add-ADGroupMember -Identity $g -Members $name
    }
    Send-LabPhase ("user $($Cfg.netbios)\$name" + $(if ($groups.Count) { ' in ' + ($groups -join ', ') } else { '' }))
}

function Invoke-LabDc($Cfg) {
    $phase = Get-LabPhase
    if ($phase -eq 'new') {
        # multi-machine lab: the machine key is the Windows computer name. Rename before promoting
        # (a promoted DC cannot be renamed), then reboot and re-enter 'new' with the name matching.
        if (Rename-LabComputer $Cfg) { return 'reboot' }
        Set-LabNetwork $Cfg
        Set-LabShutdownFriendly
        Send-LabPhase 'installing the AD DS role'
        $f = Install-WindowsFeature -Name AD-Domain-Services -IncludeManagementTools
        if (-not $f.Success) { throw "Install-WindowsFeature AD-Domain-Services failed (exit code $($f.ExitCode))" }
        Import-Module ADDSDeployment
        Send-LabPhase "creating forest $($Cfg.domain) (NetBIOS $($Cfg.netbios)), a few minutes"
        $r = Install-ADDSForest -DomainName ([string]$Cfg.domain) -DomainNetbiosName ([string]$Cfg.netbios) -InstallDns `
            -SafeModeAdministratorPassword (ConvertTo-LabSecure ([string]$Cfg.admin_password)) `
            -NoRebootOnCompletion -Force -WarningAction SilentlyContinue
        if ($r -ne $null -and $r.Status -ne $null -and [string]$r.Status -ne 'Success') {
            throw "Install-ADDSForest: $($r.Status) $($r.Message)"
        }
        # the local Administrator is now the domain's Administrator (same password)
        Set-LabAutoLogon ([string]$Cfg.netbios) 'Administrator' ([string]$Cfg.admin_password)
        Set-LabPhase 'promoted'
        Send-WtlEvent 'LAB_REBOOT' @('phase', 'promoted')
        Restart-Computer -Force
        return 'reboot'
    }

    # right after boot AD Web Services is not up yet: no AD: drive, and retry until it answers
    $env:ADPS_LoadDefaultDrive = 0
    $deadline = (Get-Date).AddMinutes(15)
    $d = $null
    while ($d -eq $null) {
        try {
            Import-Module ActiveDirectory -ErrorAction Stop
            $d = Get-ADDomain -ErrorAction Stop
        } catch {
            if ((Get-Date) -ge $deadline) { throw "Active Directory is not answering: $($_.Exception.Message)" }
            Start-Sleep -Seconds 10
        }
    }
    if ($phase -eq 'promoted') {
        foreach ($u in @($Cfg.users)) { New-LabUser $Cfg $u }
        Set-LabPhase 'ready'
    }
    Send-WtlEvent 'LAB_DC_READY' @('domain', $d.DNSRoot, 'netbios', $d.NetBIOSName, 'dc', $env:COMPUTERNAME, 'ip', $Cfg.ip)
    return 'ready'
}

# ---------------------------------------------------------------- client

function Wait-LabDc($Cfg) {
    $srv = "_ldap._tcp.dc._msdcs.$($Cfg.domain)"
    $deadline = (Get-Date).AddMinutes(60)
    $n = 0
    while ($true) {
        try {
            $r = Resolve-DnsName -Name $srv -Type SRV -Server ([string]$Cfg.dc_ip) -DnsOnly -QuickTimeout -ErrorAction Stop
            if ($r) { return }
        } catch { }
        if ((Get-Date) -ge $deadline) { throw "the DC $($Cfg.dc_ip) did not answer $srv within 60 min" }
        if ($n % 10 -eq 0) { Send-LabPhase "waiting for the domain controller $($Cfg.dc_ip)" }
        $n++
        Start-Sleep -Seconds 6
    }
}

# Register the SYSTEM worker (labsvc.ps1) to run at every boot. Done while we are still
# Administrator (the join phase); the limited LAB\student later talks to it via C:\WTL\svc.
# labsvc.ps1 ships on the lab CD next to lab.ps1; copy it to the fixed baked path first.
function Register-LabSvc([string]$LabDir) {
    $src = Join-Path $LabDir 'labsvc.ps1'
    if (Test-Path -LiteralPath $src) { Copy-Item -LiteralPath $src -Destination 'C:\WTL\labsvc.ps1' -Force }
    $cmd = 'powershell.exe -NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File C:\WTL\labsvc.ps1'
    Invoke-LabNative 'schtasks.exe' @('/Create', '/TN', 'WTL-Svc', '/TR', $cmd, '/SC', 'ONSTART',
        '/RU', 'SYSTEM', '/RL', 'HIGHEST', '/F')
    Invoke-LabNative 'schtasks.exe' @('/Run', '/TN', 'WTL-Svc')
}

function Invoke-LabClient($Cfg, [string]$LabDir) {
    $phase = Get-LabPhase
    $user = [string]$Cfg.logon_user
    if ($phase -eq 'new') {
        Set-LabNetwork $Cfg
        Set-LabShutdownFriendly
        # the agent runs as the domain user from now on: it writes its log and output under C:\WTL
        Invoke-LabNative 'icacls.exe' @('C:\WTL', '/grant', '*S-1-5-32-545:(OI)(CI)M', '/T', '/Q')
        Register-LabSvc $LabDir
        # programs are unpacked on the user's desktop; Defender (2016+) must not eat them
        if (Get-Command Add-MpPreference -ErrorAction SilentlyContinue) {
            try { Add-MpPreference -ExclusionPath 'C:\Users' -ErrorAction Stop } catch { Send-LabPhase "Defender exclusion failed: $($_.Exception.Message)" }
        }
        Wait-LabDc $Cfg
        $cred = New-Object System.Management.Automation.PSCredential(([string]$Cfg.netbios + '\Administrator'), (ConvertTo-LabSecure ([string]$Cfg.admin_password)))
        # multi-machine lab: rename to the machine key as we join (one reboot does both) so the
        # Windows computer name matches the lab's machine name.
        $join = @{ DomainName = [string]$Cfg.domain; Credential = $cred; Force = $true; ErrorAction = 'Stop'; WarningAction = 'SilentlyContinue' }
        $want = [string]$Cfg.machine
        if ($want -and $env:COMPUTERNAME -ne $want) { $join['NewName'] = $want }
        # DNS answers before the new DC finishes its first boot ("Applying computer settings" takes
        # 5-10 min on 2016/2019): keep trying for 20 min
        for ($i = 1; ; $i++) {
            try {
                Add-Computer @join
                break
            } catch {
                if ($i -ge 40) { throw "joining $($Cfg.domain) failed: $($_.Exception.Message)" }
                if ($i % 4 -eq 1) { Send-LabPhase "join attempt $i failed (DC still starting?): $($_.Exception.Message)" }
                Start-Sleep -Seconds 30
            }
        }
        Set-LabAutoLogon ([string]$Cfg.netbios) $user ([string]$Cfg.logon_password)
        # the first domain logon has no cached credentials: wait for the network before logging on
        $pol = 'HKLM:\SOFTWARE\Policies\Microsoft\Windows NT\CurrentVersion\Winlogon'
        if (-not (Test-Path $pol)) { New-Item -Path $pol -Force | Out-Null }
        New-ItemProperty -Path $pol -Name 'SyncForegroundPolicy' -Value 1 -PropertyType DWord -Force | Out-Null
        Set-LabPhase 'joined'
        Send-WtlEvent 'LAB_JOINED' @('domain', $Cfg.domain, 'autologon', "$($Cfg.netbios)\$user")
        Restart-Computer -Force
        return 'reboot'
    }

    # boots from the 0 state start DC and client together: the program should find the domain up
    Wait-LabDc $Cfg
    $who = "$env:USERDOMAIN\$env:USERNAME"
    $cs = Get-WmiObject -Class Win32_ComputerSystem
    if (-not $cs.PartOfDomain) { throw 'this computer is not in a domain' }
    if ($env:USERDOMAIN -ne $Cfg.netbios -or $env:USERNAME -ne $user) {
        throw "logged on as $who, expected $($Cfg.netbios)\$user (autologon)"
    }
    $dc = ''
    try { $dc = [System.DirectoryServices.ActiveDirectory.Domain]::GetCurrentDomain().FindDomainController().Name } catch {
        Send-LabPhase "DC lookup failed: $($_.Exception.Message)"
    }
    Send-WtlEvent 'LAB_CLIENT_READY' @('user', $who, 'domain', $cs.Domain, 'computer', $env:COMPUTERNAME, 'dc', $dc, 'ip', $Cfg.ip)
    return 'ready'
}

# ---------------------------------------------------------------- mounts + run steps
# ---- talk to the SYSTEM worker (labsvc.ps1) through C:\WTL\svc ----------------
$SvcDir = 'C:\WTL\svc'

function Invoke-SvcRequest($Req, [int]$TimeoutS = 120) {
    if (-not (Test-Path $SvcDir)) { New-Item -ItemType Directory -Path $SvcDir -Force | Out-Null }
    $id = [Guid]::NewGuid().ToString('N')
    $reqPath = Join-Path $SvcDir ($id + '.req.json')
    $resPath = Join-Path $SvcDir ($id + '.res.json')
    [System.IO.File]::WriteAllText($reqPath, ($Req | ConvertTo-Json -Compress))
    $deadline = [DateTime]::Now.AddSeconds($TimeoutS)
    while (-not (Test-Path -LiteralPath $resPath)) {
        if ([DateTime]::Now -ge $deadline) { throw "the privileged worker did not answer within ${TimeoutS}s (is WTL-Svc running?)" }
        Start-Sleep -Milliseconds 300
    }
    Start-Sleep -Milliseconds 100
    $res = [System.IO.File]::ReadAllText($resPath) | ConvertFrom-Json
    try { Remove-Item -LiteralPath $resPath -Force } catch { }
    return $res
}

# Assign each mounted disc its fixed drive letter (needs privilege -> the SYSTEM worker).
function Set-LabMounts($Mounts) {
    foreach ($m in @($Mounts)) {
        $letter = ([string]$m.letter).ToUpper()
        $res = Invoke-SvcRequest @{ action = 'mount'; letter = $letter }
        if (-not $res.ok) { throw "mount ${letter}: failed: $($res.error)" }
        Send-WtlEvent 'LAB_MOUNT' @('letter', $res.letter, 'host', [string]$m.host)
    }
}

# Run one step, capture its combined output, report LAB_STEP + the exit code over serial.
# $Who 'admin' -> the SYSTEM worker runs it (the lab's root); 'student' -> run here as LAB\student.
function Invoke-LabStep([string]$Who, [int]$N, [string]$Path, [int]$TimeoutS) {
    $runDir = 'C:\WTL\run'
    if (-not (Test-Path $runDir)) { New-Item -ItemType Directory -Path $runDir -Force | Out-Null }
    $out = Join-Path $runDir ("step$N.out"); $err = Join-Path $runDir ("step$N.err")
    $cwd = [System.IO.Path]::GetDirectoryName($Path)
    if (-not $cwd) { $cwd = ($Path.Substring(0, 2) + '\') }
    $exitCode = $null; $timedOut = $false; $secs = 0

    if ($Who -eq 'admin') {
        $res = Invoke-SvcRequest @{ action = 'run'; path = $Path; cwd = $cwd; timeout = $TimeoutS;
                                    out = $out; err = $err } ($TimeoutS + 60)
        if (-not $res.ok) {
            Send-WtlEvent 'LAB_STEP' @('as', $Who, 'n', $N, 'script', $Path, 'exit', 'launchfail') ([string]$res.error)
            return 1
        }
        if ($res.timed_out) { $timedOut = $true } else { $exitCode = [int]$res.exit }
        $secs = $res.secs
    } else {
        $inF = Join-Path $runDir 'empty.in'
        foreach ($f in @($out, $err)) { if (Test-Path $f) { Remove-Item $f -Force } }
        [System.IO.File]::WriteAllText($inF, '')
        $ext = [System.IO.Path]::GetExtension($Path).ToLowerInvariant()
        $file = $Path; $argLine = ''
        if ($ext -eq '.ps1') {
            $file = 'powershell.exe'; $argLine = '-NoProfile -ExecutionPolicy Bypass -File "' + $Path + '"'
        } elseif ($ext -eq '.bat' -or $ext -eq '.cmd') {
            $file = $env:ComSpec; $argLine = '/d /c ""' + $Path + '""'
        }
        $sp = @{ FilePath = $file; PassThru = $true; NoNewWindow = $true;
                 RedirectStandardOutput = $out; RedirectStandardError = $err; RedirectStandardInput = $inF }
        if (Test-Path -LiteralPath $cwd) { $sp['WorkingDirectory'] = $cwd }
        if ($argLine) { $sp['ArgumentList'] = $argLine }
        $t0 = [DateTime]::Now
        $proc = $null
        try { $proc = Start-Process @sp } catch {
            Send-WtlEvent 'LAB_STEP' @('as', $Who, 'n', $N, 'script', $Path, 'exit', 'launchfail') $_.Exception.Message
            return 1
        }
        try { $null = $proc.Handle } catch { }
        $deadline = $t0.AddSeconds([Math]::Max(5, $TimeoutS))
        while (-not $proc.HasExited) {
            if ([DateTime]::Now -ge $deadline) { $timedOut = $true; break }
            Start-Sleep -Milliseconds 250
        }
        if ($timedOut) { Stop-Tree $proc; Start-Sleep -Seconds 1 } else { $exitCode = Get-ExitCodeSafe $proc }
        $secs = [Math]::Round(([DateTime]::Now - $t0).TotalSeconds, 1)
    }

    Send-Output ("step" + $N + "out") $out
    Send-Output ("step" + $N + "err") $err
    if ($timedOut) {
        Send-WtlEvent 'LAB_STEP' @('as', $Who, 'n', $N, 'script', $Path, 'exit', 'timeout', 'secs', $secs)
        return 2
    }
    Send-WtlEvent 'LAB_STEP' @('as', $Who, 'n', $N, 'script', $Path, 'exit', $exitCode, 'secs', $secs)
    return 0
}

# Per-run work on the client (runs as LAB\student; admin steps go to the SYSTEM worker).
# No reboots: mounts and root steps are delegated, so the student session stays put.
# Steps run IN THE ORDER the config wrote them - run_as_admin and run_as_student may interleave.
function Invoke-LabRun($Cfg) {
    $run = $Cfg.run
    if ($run -eq $null) { Send-WtlEvent 'LAB_RUN_DONE' @('admin', 0, 'student', 0) 'no run steps'; return 'done' }
    $steps = @($run.steps | Where-Object { $_ })
    $wait = 60; if ($run.wait) { try { $wait = [int]$run.wait } catch { } }

    Set-LabMounts $run.mounts

    $n = 0; $nAdmin = 0; $nStudent = 0
    foreach ($s in $steps) {
        $who = [string]$s.who
        $n++
        if ($who -eq 'admin') { $nAdmin++ } else { $nStudent++ }
        [void](Invoke-LabStep $who $n ([string]$s.path) $wait)
    }
    Send-WtlEvent 'LAB_RUN_DONE' @('admin', $nAdmin, 'student', $nStudent)
    return 'done'
}

# ---------------------------------------------------------------- entry

# ================================================================ multi-machine labs (fmt=multi)
#
# A multi-machine lab is one DC + N domain-joined members on one private multicast LAN. The DC
# role is as in the two-machine lab but creates every lab user; a member joins, sets its own
# autologon user, and (on a run) runs the steps the host put on its control disc. A step's `as`
# is "system" (the SYSTEM worker), "logon" (this machine's own logged-on user, foreground), or
# "user" (another account, launched by the worker with its credentials). Each LAB_STEP carries
# machine= so the host can tell the machines' reports apart.

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

# Run one step on this machine. $S has: as (system|logon|user), user, password, path, args,
# keep_running. Reports LAB_STEP machine=.. as=.. n=.. script=.. exit=.. [secs=..].
function Invoke-LabMultiStep($Cfg, [string]$Machine, [int]$N, $S, [int]$TimeoutS) {
    $as = [string]$S.as_
    if (-not $as) { $as = [string]$S.'as' }   # defensive: ConvertFrom-Json key may be 'as_' or 'as'
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
        $req['domain'] = [string]$Cfg.netbios
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

# Run a step in the current (logged-on) session, capturing output. Mirrors the student branch
# of the two-machine Invoke-LabStep, with args and keep_running.
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
    $role = [string]$Cfg.role
    if ($role -eq 'dc') {
        # the DC uses the same promotion path; Invoke-LabDc reads domain/netbios/users/ip/dc_ip
        $r = Invoke-LabDc $Cfg
        if ($r -ne 'ready') { return $r }
        # the DC also needs the SYSTEM worker baked, so it can run its own steps
        Register-LabSvc $LabDir
        if ($Cfg.run -ne $null) { return (Invoke-LabMemberSteps $Cfg) }
        return 'ready'
    }
    if ($role -ne 'member') { throw "unknown role '$role' in lab.json (fmt=multi)" }
    # a member: join + autologon (phase 'new'), else it is logged on -> ready / run steps
    $r = Invoke-LabClient $Cfg $LabDir
    if ($r -ne 'ready') { return $r }
    if ($Cfg.shutdown_when_ready) { Send-WtlEvent 'LAB_IDLE'; return 'reboot' }
    if ($Cfg.run -ne $null) { return (Invoke-LabMemberSteps $Cfg) }
    return 'ready'
}

# ================================================================ entry

function Invoke-Lab([string]$LabDir) {
    try {
        $cfg = [System.IO.File]::ReadAllText((Join-Path $LabDir 'lab.json')) | ConvertFrom-Json
        Send-WtlEvent 'LAB_START' @('role', $cfg.role, 'phase', (Get-LabPhase), 'user', "$env:USERDOMAIN\$env:USERNAME")
        if ([string]$cfg.fmt -eq 'multi') { return (Invoke-LabMulti $cfg $LabDir) }
        $r = 'fail'
        if ($cfg.role -eq 'dc') { $r = Invoke-LabDc $cfg }
        elseif ($cfg.role -eq 'client') { $r = Invoke-LabClient $cfg $LabDir }
        else { throw "unknown role '$($cfg.role)' in lab.json" }
        if ($r -eq 'ready' -and $cfg.role -eq 'client') {
            # lab.sh prepare: the finished machine is the lab's 0 state; the host powers it off via
            # ACPI (a plain domain user may not call Stop-Computer; ShutdownWithoutLogon covers it)
            if ($cfg.shutdown_when_ready) { Send-WtlEvent 'LAB_IDLE'; return 'reboot' }
            # a run: do the mounts + admin/student steps (may reboot and resume)
            if ($cfg.run -ne $null) { return (Invoke-LabRun $cfg) }
        }
        return $r
    } catch {
        $msg = $_.Exception.Message
        try { $msg = "$msg (line $($_.InvocationInfo.ScriptLineNumber))" } catch { }
        Send-WtlEvent 'LAB_FAIL' @() $msg
        return 'fail'
    }
}
