# lab.ps1 - win-test-lab AD lab, guest side.
#
# PowerShell 4.0 compatible (Server 2012 R2 baseline). No PS5+ features.
# Dot-sourced by agent.ps1 when a CD has \LAB\lab.ps1 + \LAB\lab.json (the control disc the
# host's `lab` writes for each machine); uses the agent's Send-WtlEvent. Runs at every logon
# and continues where it left off:
#
#   role dc:      rename to the machine key -> reboot -> static IP on the lab NIC -> AD DS
#                 role + new forest -> reboot -> (LAB\Administrator) users + groups
#                 -> WTL LAB_DC_READY [-> its own run steps]
#   role member:  static IP, DNS = DC -> wait for the DC -> join the domain and rename to the
#                 machine key, autologon = its logon user -> reboot -> (logged on as e.g.
#                 LAB\student1) WTL LAB_CLIENT_READY [-> its run steps]
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
# if the name already matches or no machine key is set.
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

# static IP on the lab NIC. In an AD lab every adapter uses the DC for DNS and registers nothing
# but the lab IP; in a workgroup the NAT adapter keeps its DHCP DNS. No lab NIC (a single
# workgroup machine): nothing to do.
function Set-LabNetwork($Cfg) {
    if (-not $Cfg.lan) { return }
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
    if (-not $Cfg.dc_ip) {
        Send-LabPhase "network: $($Cfg.ip)/$($Cfg.prefix) on the lab NIC (workgroup)"
        return
    }
    foreach ($n in @(Get-NetAdapter -ErrorAction SilentlyContinue)) {
        Set-DnsClientServerAddress -InterfaceIndex $n.ifIndex -ServerAddresses @([string]$Cfg.dc_ip)
        if ($n.ifIndex -ne $if) { Set-DnsClient -InterfaceIndex $n.ifIndex -RegisterThisConnectionsAddress $false -ErrorAction SilentlyContinue }
    }
    Send-LabPhase "network: $($Cfg.ip)/$($Cfg.prefix) on the lab NIC, DNS $($Cfg.dc_ip)"
}

# The local Administrator gets the lab's admin_password (the base image has the default one),
# and the next autologon uses it. On the DC this becomes the domain Administrator's password.
function Set-LabLocalAdmin($Cfg) {
    $pw = [string]$Cfg.admin_password
    if (-not $pw) { return }
    Invoke-LabNative 'net.exe' @('user', 'Administrator', $pw)
    Set-LabAutoLogon $env:COMPUTERNAME 'Administrator' $pw
}

# Run a native command only for its exit code (its output, stderr included, is dropped). The
# agent runs with ErrorActionPreference=Stop, under which a native command's stderr would throw.
function Test-LabNative([string]$Exe, [string[]]$Arguments) {
    $ErrorActionPreference = 'Continue'
    $null = & $Exe @Arguments 2>&1
    return ($LASTEXITCODE -eq 0)
}

# Workgroup mode: the lab's users as local accounts on this machine, in their (local) groups;
# a group that does not exist is created. Passwords never expire.
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
        Send-LabPhase ("local user $name" + $(if ($groups.Count) { ' in ' + ($groups -join ', ') } else { '' }))
    }
}

# ---------------------------------------------------------------- roles / features / setup
#
# Baked into the 0 state during the build. A member installs these in the "configure" phase,
# logged on as the domain (or local) Administrator; the DC installs them inline after it has
# created the users (it is already the domain Administrator). ad-ds is the DC promotion itself
# and is skipped here. An unknown feature fails the build with the available list.

function Measure-LabList($List) {
    if ($List -eq $null) { return 0 }
    return @($List | Where-Object { $_ -ne $null }).Count
}

# True if this machine has build-time configuration (roles, features or setup scripts) to apply.
function Test-LabHasConfig($Cfg) {
    return ((Measure-LabList $Cfg.roles) -gt 0 -or (Measure-LabList $Cfg.features) -gt 0 -or (Measure-LabList $Cfg.setup) -gt 0)
}

function Install-LabFeatures($Cfg) {
    $feats = @($Cfg.features | Where-Object { $_ })
    if ($feats.Count -eq 0) { return }
    foreach ($f in $feats) {
        if (-not (Get-WindowsFeature -Name $f -ErrorAction SilentlyContinue)) {
            $avail = (Get-WindowsFeature | Where-Object { $_.Name } | ForEach-Object { [string]$_.Name }) -join ', '
            throw "feature '$f' is not available on this OS. Available features: $avail"
        }
    }
    Send-LabPhase ("installing features: " + ($feats -join ', '))
    $r = Install-WindowsFeature -Name $feats -IncludeManagementTools
    if (-not $r.Success) { throw ("Install-WindowsFeature (" + ($feats -join ', ') + ") failed, exit code " + $r.ExitCode) }
    foreach ($f in $feats) { Send-WtlEvent 'LAB_FEATURE' @('name', $f) }
}

function Install-LabRoles($Cfg) {
    foreach ($role in @($Cfg.roles | Where-Object { $_ })) {
        switch ([string]$role) {
            'ad-ds' { }   # the DC promotion already installed AD DS
            'ad-cs' { Install-LabAdCs $Cfg }
            'dhcp'  { Install-LabDhcp $Cfg }
            'iis'   { Install-LabIis $Cfg }
            default { throw "unknown role '$role'" }
        }
    }
}

# AD CS Enterprise Root CA (needs a domain; runs as a domain/enterprise admin). Idempotent: a
# second build run finds the CA already installed and only (re)publishes templates + the GPO.
function Install-LabAdCs($Cfg) {
    $o = $Cfg.ad_cs
    $caName = [string]$o.ca_name
    $keyLen = 2048; if ($o.key_length) { try { $keyLen = [int]$o.key_length } catch { } }
    $years = 5; if ($o.validity_years) { try { $years = [int]$o.validity_years } catch { } }
    Send-WtlEvent 'LAB_ROLE' @('name', 'ad-cs', 'state', 'installing', 'ca', $caName)
    $f = Install-WindowsFeature -Name ADCS-Cert-Authority -IncludeManagementTools
    if (-not $f.Success) { throw "Install-WindowsFeature ADCS-Cert-Authority failed, exit code $($f.ExitCode)" }
    Import-Module ADCSDeployment -ErrorAction SilentlyContinue
    try {
        Install-AdcsCertificationAuthority -CAType EnterpriseRootCA -CACommonName $caName `
            -KeyLength $keyLen -HashAlgorithmName SHA256 -ValidityPeriod Years -ValidityPeriodUnits $years `
            -Force -ErrorAction Stop
    } catch {
        $m = [string]$_.Exception.Message
        # 0x80070542 / "already installed": fine on a rebuild
        if ($m -notmatch 'already installed' -and $m -notmatch '0x80070542') { throw "Install-AdcsCertificationAuthority: $m" }
    }
    # wait for the CA service to answer
    for ($i = 0; $i -lt 30; $i++) {
        if (Test-LabNative 'certutil.exe' @('-ping')) { break }
        Start-Sleep -Seconds 2
    }
    if ($o.autoenroll) { Enable-LabAutoEnroll $Cfg }
    Send-WtlEvent 'LAB_ROLE' @('name', 'ad-cs', 'state', 'ready', 'ca', $caName)
}

# Publish the Workstation + User templates on the CA and link a domain GPO that turns on
# computer and user certificate auto-enrollment. The default template ACLs already grant
# Domain Computers / Domain Users Enroll + Autoenroll, so members pull certs on gpupdate.
$LabEnrollRight = [Guid]'0e10c968-78fb-11d2-90d4-00c04f79dc55'       # Certificate-Enrollment
$LabAutoEnrollRight = [Guid]'a05b8cc2-17bc-4802-a710-e7c15ab866a2'   # Certificate-AutoEnrollment

# Let GROUP (e.g. "Domain Users") read, enroll for and auto-enroll for the template at DN.
function Grant-LabTemplateEnroll([string]$Dn, [string]$Netbios, [string]$Group) {
    $sid = (New-Object System.Security.Principal.NTAccount("$Netbios\$Group")).Translate([System.Security.Principal.SecurityIdentifier])
    $t = New-Object System.DirectoryServices.DirectoryEntry("LDAP://$Dn")
    $sec = $t.ObjectSecurity
    $sec.AddAccessRule((New-Object System.DirectoryServices.ActiveDirectoryAccessRule($sid, 'GenericRead', 'Allow')))
    foreach ($g in @($LabEnrollRight, $LabAutoEnrollRight)) {
        $sec.AddAccessRule((New-Object System.DirectoryServices.ActiveDirectoryAccessRule($sid, 'ExtendedRight', 'Allow', $g)))
    }
    $t.ObjectSecurity = $sec
    $t.CommitChanges()
}

# The built-in User template is version 1, which Windows never auto-enrolls, and it requires an
# e-mail address. Make a version 2 copy, "WTLUser" (subject from the directory path, UPN in the
# SAN, auto-enrollment on), with its own template OID. ADSI only: no AD module on a member.
function New-LabUserTemplate([string]$Netbios) {
    $cfgNc = [string]([ADSI]'LDAP://RootDSE').configurationNamingContext
    $pks = "CN=Public Key Services,CN=Services,$cfgNc"
    $dn = "CN=WTLUser,CN=Certificate Templates,$pks"
    if ([System.DirectoryServices.DirectoryEntry]::Exists("LDAP://$dn")) { return }
    $src = New-Object System.DirectoryServices.DirectoryEntry("LDAP://CN=User,CN=Certificate Templates,$pks")
    $oidRoot = [ADSI]"LDAP://CN=OID,$pks"
    $rnd = New-Object System.Random
    $oid = [string]$oidRoot.psbase.Properties['msPKI-Cert-Template-OID'].Value + '.' + $rnd.Next(1000000, 99999999) + '.' + $rnd.Next(1000000, 99999999)
    # the template's OID object, as the Certificate Templates console creates it
    $o = $oidRoot.Create('msPKI-Enterprise-Oid', 'CN=' + $rnd.Next(1000000, 99999999) + '.' + ([Guid]::NewGuid().ToString('N').ToUpper()))
    $o.Put('flags', 1)
    $o.Put('displayName', 'WTL User')
    $o.Put('msPKI-Cert-Template-OID', $oid)
    $o.SetInfo()
    $c = ([ADSI]"LDAP://CN=Certificate Templates,$pks").Create('pKICertificateTemplate', 'CN=WTLUser')
    foreach ($a in @('pKIDefaultKeySpec', 'pKIKeyUsage', 'pKIMaxIssuingDepth', 'pKICriticalExtensions', 'pKIExpirationPeriod',
                     'pKIOverlapPeriod', 'pKIExtendedKeyUsage', 'pKIDefaultCSPs', 'msPKI-RA-Signature', 'msPKI-Private-Key-Flag')) {
        $v = $src.Properties[$a].Value
        if ($v -ne $null) { $c.Put($a, $v) }
    }
    $c.Put('displayName', 'WTL User')
    $c.Put('revision', 100)
    # not IS_DEFAULT (0x10000) any more, IS_MODIFIED (0x20000), AUTO_ENROLLMENT (0x20)
    $c.Put('flags', (([int]$src.Properties['flags'].Value -band (-bnot 0x10000)) -bor 0x20000 -bor 0x20))
    $c.Put('msPKI-Template-Schema-Version', 2)
    $c.Put('msPKI-Template-Minor-Revision', 1)
    $c.Put('msPKI-Cert-Template-OID', $oid)
    $c.Put('msPKI-Minimal-Key-Size', 2048)
    $c.Put('msPKI-Enrollment-Flag', 0x29)                    # symmetric algs, publish to DS, auto-enroll
    $c.Put('msPKI-Certificate-Name-Flag', -2113929216)      # 0x82000000: SAN UPN + subject = DN
    $c.Put('msPKI-Certificate-Application-Policy', $src.Properties['pKIExtendedKeyUsage'].Value)
    $c.SetInfo()
    Grant-LabTemplateEnroll $dn $Netbios 'Domain Users'
    Send-LabPhase 'certificate template WTLUser (v2 copy of User, auto-enroll) created'
}

function Enable-LabAutoEnroll($Cfg) {
    $netbios = [string]$Cfg.netbios
    $pks = 'CN=Public Key Services,CN=Services,' + [string]([ADSI]'LDAP://RootDSE').configurationNamingContext
    New-LabUserTemplate $netbios
    # Workstation Authentication (v2) for computers: make sure Domain Computers may auto-enroll
    Grant-LabTemplateEnroll "CN=Workstation,CN=Certificate Templates,$pks" $netbios 'Domain Computers'
    # Publishing a template = adding its name to the CA's enrollment-service object in AD
    # (what Add-CATemplate / certutil -SetCATemplates do too). Written with ADSI, so a freshly
    # created template the CA's cache has not seen yet is no obstacle; certsvc reloads it below.
    $ca = [string]$Cfg.ad_cs.ca_name
    $es = New-Object System.DirectoryServices.DirectoryEntry("LDAP://CN=$ca,CN=Enrollment Services,$pks")
    $last = ''
    for ($i = 1; $i -le 12; $i++) {
        try {
            $have = @($es.Properties['certificateTemplates'] | ForEach-Object { [string]$_ })
            foreach ($t in @('Workstation', 'WTLUser')) {
                if ($have -notcontains $t) { [void]$es.Properties['certificateTemplates'].Add($t) }
            }
            $es.CommitChanges()
            $es.RefreshCache()
            $now = @($es.Properties['certificateTemplates'] | ForEach-Object { [string]$_ })
            if (($now -contains 'Workstation') -and ($now -contains 'WTLUser')) { $last = ''; break }
            $last = 'the templates are not in certificateTemplates after the write: ' + ($now -join ', ')
        } catch { $last = $_.Exception.Message }
        Send-LabPhase "publishing templates on $ca, attempt ${i}: $last"
        Start-Sleep -Seconds 10
    }
    if ($last) { throw "publishing the certificate templates Workstation + WTLUser on $ca failed: $last" }
    # the CA reads its template list at start
    Restart-Service certsvc -Force
    for ($i = 0; $i -lt 30; $i++) {
        if (Test-LabNative 'certutil.exe' @('-ping')) { break }
        Start-Sleep -Seconds 2
    }
    # the GPO cmdlets come from GPMC; add it if the CA machine does not have it yet
    if (-not (Get-Module -ListAvailable -Name GroupPolicy)) {
        $g = Install-WindowsFeature -Name GPMC
        if (-not $g.Success) { throw "Install-WindowsFeature GPMC failed, exit code $($g.ExitCode) (needed for the auto-enrollment GPO)" }
    }
    Import-Module GroupPolicy -ErrorAction SilentlyContinue
    $gpoName = 'WTL Certificate Auto-Enrollment'
    $gpo = Get-GPO -Name $gpoName -ErrorAction SilentlyContinue
    if (-not $gpo) { $gpo = New-GPO -Name $gpoName }
    # AEPolicy 7 = enabled + renew expiring + update templates (computer and user)
    Set-GPRegistryValue -Name $gpoName -Key 'HKLM\Software\Policies\Microsoft\Cryptography\AutoEnrollment' -ValueName 'AEPolicy' -Type DWord -Value 7 | Out-Null
    Set-GPRegistryValue -Name $gpoName -Key 'HKCU\Software\Policies\Microsoft\Cryptography\AutoEnrollment' -ValueName 'AEPolicy' -Type DWord -Value 7 | Out-Null
    # the domain's DN from RootDSE (a member has no ActiveDirectory module)
    $domainDn = [string]([ADSI]'LDAP://RootDSE').defaultNamingContext
    try { New-GPLink -Name $gpoName -Target $domainDn -LinkEnabled Yes -ErrorAction Stop | Out-Null } catch {
        if ("$($_.Exception.Message)" -notmatch 'already linked') { throw "linking the auto-enrollment GPO to ${domainDn}: $($_.Exception.Message)" }
    }
    Send-LabPhase "auto-enrollment GPO linked to $domainDn; templates Workstation + WTLUser published"
}

# DHCP server: a scope for the lab subnet, router + DNS = the DC, authorized in AD when there
# is a domain.
function Install-LabDhcp($Cfg) {
    $d = $Cfg.dhcp
    $start = [string]$d.start; $end = [string]$d.end; $mask = [string]$d.mask; $router = [string]$d.router
    Send-WtlEvent 'LAB_ROLE' @('name', 'dhcp', 'state', 'installing', 'scope', "$start-$end")
    $f = Install-WindowsFeature -Name DHCP -IncludeManagementTools
    if (-not $f.Success) { throw "Install-WindowsFeature DHCP failed, exit code $($f.ExitCode)" }
    Import-Module DhcpServer -ErrorAction SilentlyContinue
    $have = Get-DhcpServerv4Scope -ErrorAction SilentlyContinue | Where-Object { [string]$_.StartRange -eq $start }
    if (-not $have) {
        Add-DhcpServerv4Scope -Name 'LabScope' -StartRange $start -EndRange $end -SubnetMask $mask -State Active -ErrorAction Stop
    }
    $scope = Get-DhcpServerv4Scope -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($scope -and $router) {
        try { Set-DhcpServerv4OptionValue -ScopeId $scope.ScopeId -Router $router -DnsServer $router -ErrorAction Stop } catch { Send-LabPhase "DHCP options: $($_.Exception.Message)" }
    }
    if ([string]$Cfg.mode -ne 'workgroup') {
        try { Add-DhcpServerInDC -DnsName ("$env:COMPUTERNAME." + [string]$Cfg.domain) -IPAddress ([string]$Cfg.ip) -ErrorAction Stop } catch {
            if ("$($_.Exception.Message)" -notmatch 'already') { Send-LabPhase "DHCP authorize: $($_.Exception.Message)" }
        }
    }
    # tell Server Manager the post-install config is done (no yellow bang on screenshots)
    try {
        $sm = 'HKLM:\SOFTWARE\Microsoft\ServerManager\Roles\12'
        if (Test-Path $sm) { Set-ItemProperty -Path $sm -Name 'ConfigurationState' -Value 2 -ErrorAction SilentlyContinue }
    } catch { }
    Send-WtlEvent 'LAB_ROLE' @('name', 'dhcp', 'state', 'ready', 'scope', "$start-$end")
}

# IIS: Web-Server + the management console (+ ASP.NET 4.5 where available), the default site
# serving a test page at http://<machine>/.
function Install-LabIis($Cfg) {
    $o = $Cfg.iis
    Send-WtlEvent 'LAB_ROLE' @('name', 'iis', 'state', 'installing')
    $feats = @('Web-Server', 'Web-Mgmt-Console', 'Web-Default-Doc')
    if (Get-WindowsFeature -Name Web-Asp-Net45 -ErrorAction SilentlyContinue) { $feats += 'Web-Asp-Net45' }
    $r = Install-WindowsFeature -Name $feats -IncludeManagementTools
    if (-not $r.Success) { throw "Install-WindowsFeature (IIS) failed, exit code $($r.ExitCode)" }
    $path = [string]$o.site_path
    if (-not $path) { $path = 'C:\inetpub\wwwroot' }
    if (-not (Test-Path -LiteralPath $path)) { New-Item -ItemType Directory -Path $path -Force | Out-Null }
    $index = Join-Path $path 'index.html'
    if (-not (Test-Path -LiteralPath $index)) {
        $html = "<html><head><title>win-test-lab</title></head><body><h1>win-test-lab</h1><p>IIS on $env:COMPUTERNAME is up.</p></body></html>"
        [System.IO.File]::WriteAllText($index, $html)
    }
    try { iisreset.exe /start | Out-Null } catch { }
    Send-WtlEvent 'LAB_ROLE' @('name', 'iis', 'state', 'ready', 'path', $path)
}

# Run the build-time setup scripts, in order, through the SYSTEM worker (so "system" has full
# rights and an "as" user runs with its own token). A non-zero exit fails the build.
function Invoke-LabSetup($Cfg, [string]$LabDir) {
    $items = @($Cfg.setup | Where-Object { $_ })
    if ($items.Count -eq 0) { return }
    $runDir = 'C:\WTL\setup'
    if (-not (Test-Path $runDir)) { New-Item -ItemType Directory -Path $runDir -Force | Out-Null }
    $n = 0
    foreach ($s in $items) {
        $n++
        $path = Join-Path $LabDir (([string]$s.file).Replace('/', '\'))
        if (-not (Test-Path -LiteralPath $path)) { throw "setup script not found on the control disc: $($s.file)" }
        $as = [string]$s.as_
        if (-not $as) { $as = 'system' }
        $out = Join-Path $runDir ("setup$n.out"); $err = Join-Path $runDir ("setup$n.err")
        $req = @{ action = 'run'; path = $path; cwd = $LabDir; timeout = 600; out = $out; err = $err; args = [string]$s.args }
        if ($as -eq 'user') {
            $req['user'] = [string]$s.user
            $req['password'] = [string]$s.password
            $req['domain'] = [string]$Cfg.netbios
            if ([string]$Cfg.mode -eq 'workgroup') { $req['domain'] = '.' }
        }
        Send-WtlEvent 'LAB_SETUP' @('n', $n, 'as', $as, 'script', ([string]$s.file), 'state', 'running')
        $res = Invoke-SvcRequest $req 660
        $tail = ''
        try { if (Test-Path $err) { $tail = (Get-Content -LiteralPath $err -ErrorAction SilentlyContinue | Select-Object -Last 5) -join '; ' } } catch { }
        if (-not $res.ok) { throw "setup script $($s.file) could not start: $($res.error)" }
        if ($res.timed_out) { throw "setup script $($s.file) timed out" }
        if ([int]$res.exit -ne 0) { throw "setup script $($s.file) failed with exit $($res.exit). $tail" }
        Send-WtlEvent 'LAB_SETUP' @('n', $n, 'as', $as, 'script', ([string]$s.file), 'state', 'ok')
    }
}

# Apply this machine's build-time configuration: roles, then features, then setup scripts.
function Invoke-LabConfigure($Cfg, [string]$LabDir) {
    Install-LabRoles $Cfg
    Install-LabFeatures $Cfg
    Invoke-LabSetup $Cfg $LabDir
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
        # already a member (e.g. "Domain Users", every user's primary group): nothing to do
        try { Add-ADGroupMember -Identity $g -Members $name -ErrorAction Stop } catch {
            if ("$($_.Exception.Message)" -notmatch 'already a member') { throw }
        }
    }
    Send-LabPhase ("user $($Cfg.netbios)\$name" + $(if ($groups.Count) { ' in ' + ($groups -join ', ') } else { '' }))
}

function Invoke-LabDc($Cfg, [string]$LabDir) {
    $phase = Get-LabPhase
    if ($phase -eq 'new') {
        # the machine key is the Windows computer name. Rename before promoting (a promoted DC
        # cannot be renamed), then reboot and re-enter 'new' with the name matching. The local
        # Administrator gets admin_password first: promotion makes it the domain Administrator.
        Set-LabLocalAdmin $Cfg
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
        # the DC is already the domain Administrator: install its own roles/features/setup now
        # (dc machines never carry a run during the build, so this is build-time only)
        if (Test-LabHasConfig $Cfg) {
            Register-LabSvc $LabDir
            Send-WtlEvent 'LAB_CONFIGURE' @('machine', $env:COMPUTERNAME, 'role', 'dc')
            Invoke-LabConfigure $Cfg $LabDir
        }
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
    $ad = ([string]$Cfg.mode -ne 'workgroup')
    # the account this machine logs on as: its logon user, else Administrator; in the domain
    # (AD) or on this machine (workgroup)
    $user = [string]$Cfg.logon_user
    $pass = [string]$Cfg.logon_password
    if (-not $user) { $user = 'Administrator'; $pass = [string]$Cfg.admin_password }
    $realm = [string]$Cfg.netbios
    if (-not $ad) { $realm = [string]$Cfg.machine }
    if ($phase -eq 'new') {
        Set-LabNetwork $Cfg
        Set-LabShutdownFriendly
        # the agent runs as the logon user from now on: it writes its log and output under C:\WTL
        Invoke-LabNative 'icacls.exe' @('C:\WTL', '/grant', '*S-1-5-32-545:(OI)(CI)M', '/T', '/Q')
        Register-LabSvc $LabDir
        # programs are unpacked on the user's desktop; Defender (2016+) must not eat them
        if (Get-Command Add-MpPreference -ErrorAction SilentlyContinue) {
            try { Add-MpPreference -ExclusionPath 'C:\Users' -ErrorAction Stop } catch { Send-LabPhase "Defender exclusion failed: $($_.Exception.Message)" }
        }
        Set-LabLocalAdmin $Cfg
        if (-not $ad) {
            # workgroup: local users + groups, rename to the machine key; one reboot
            New-LabLocalUsers $Cfg
            $want = [string]$Cfg.machine
            if ($want -and $env:COMPUTERNAME -ne $want) {
                Send-LabPhase "renaming this computer $env:COMPUTERNAME -> $want"
                Rename-Computer -NewName $want -Force -ErrorAction Stop -WarningAction SilentlyContinue
            }
            # a machine with build config stays Administrator for the configure phase, then
            # switches to its final logon user; otherwise it logs straight on as that user
            if (Test-LabHasConfig $Cfg) {
                Set-LabAutoLogon $realm 'Administrator' ([string]$Cfg.admin_password)
                Set-LabPhase 'configure'
            } else {
                Set-LabAutoLogon $realm $user $pass
                Set-LabPhase 'joined'
            }
            Send-WtlEvent 'LAB_JOINED' @('workgroup', $want, 'autologon', "$realm\$user")
            Restart-Computer -Force
            return 'reboot'
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
                # an attempt can join and still report an error (a timeout after the DC answered):
                # then every retry says "already in that domain". Joined is joined; finish the rename.
                $now = Get-WmiObject -Class Win32_ComputerSystem
                if ($now.PartOfDomain -and ([string]$now.Domain -eq [string]$Cfg.domain)) {
                    Send-LabPhase "joined $($Cfg.domain) (an earlier attempt succeeded: $($_.Exception.Message))"
                    $pending = [string](Get-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Control\ComputerName\ComputerName').ComputerName
                    if ($want -and $pending -ne $want) {
                        Rename-Computer -NewName $want -DomainCredential $cred -Force -ErrorAction Stop -WarningAction SilentlyContinue
                    }
                    break
                }
                if ($i -ge 40) { throw "joining $($Cfg.domain) failed: $($_.Exception.Message)" }
                if ($i % 4 -eq 1) { Send-LabPhase "join attempt $i failed (DC still starting?): $($_.Exception.Message)" }
                Start-Sleep -Seconds 30
            }
        }
        # a machine with build config stays the domain Administrator for the configure phase
        # (roles like ad-cs/dhcp need that token), then switches to its final logon user
        if (Test-LabHasConfig $Cfg) {
            Set-LabAutoLogon ([string]$Cfg.netbios) 'Administrator' ([string]$Cfg.admin_password)
        } else {
            Set-LabAutoLogon $realm $user $pass
        }
        # the first domain logon has no cached credentials: wait for the network before logging on
        $pol = 'HKLM:\SOFTWARE\Policies\Microsoft\Windows NT\CurrentVersion\Winlogon'
        if (-not (Test-Path $pol)) { New-Item -Path $pol -Force | Out-Null }
        New-ItemProperty -Path $pol -Name 'SyncForegroundPolicy' -Value 1 -PropertyType DWord -Force | Out-Null
        if (Test-LabHasConfig $Cfg) { Set-LabPhase 'configure' } else { Set-LabPhase 'joined' }
        Send-WtlEvent 'LAB_JOINED' @('domain', $Cfg.domain, 'autologon', "$realm\$user")
        Restart-Computer -Force
        return 'reboot'
    }

    # phase 'configure' (build only): logged on as the Administrator, apply roles/features/setup,
    # then switch autologon to the final logon user and reboot into 'joined'
    if ($phase -eq 'configure') {
        Wait-LabDc $Cfg
        Send-WtlEvent 'LAB_CONFIGURE' @('machine', $env:COMPUTERNAME, 'role', 'member')
        Invoke-LabConfigure $Cfg $LabDir
        Set-LabAutoLogon $realm $user $pass
        Set-LabPhase 'joined'
        Send-WtlEvent 'LAB_CONFIGURED' @('machine', $env:COMPUTERNAME, 'autologon', "$realm\$user")
        Restart-Computer -Force
        return 'reboot'
    }

    $who = "$env:USERDOMAIN\$env:USERNAME"
    $cs = Get-WmiObject -Class Win32_ComputerSystem
    if ($env:USERDOMAIN -ne $realm -or $env:USERNAME -ne $user) {
        throw "logged on as $who, expected $realm\$user (autologon)"
    }
    if (-not $ad) {
        Send-WtlEvent 'LAB_CLIENT_READY' @('user', $who, 'workgroup', $cs.Workgroup, 'computer', $env:COMPUTERNAME, 'ip', $Cfg.ip)
        return 'ready'
    }
    # boots from the 0 state start DC and client together: the program should find the domain up
    Wait-LabDc $Cfg
    if (-not $cs.PartOfDomain) { throw 'this computer is not in a domain' }
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

# ---------------------------------------------------------------- run steps
#
# An AD lab is one DC + N domain-joined members; a workgroup lab is standalone machines with
# local users. Every machine runs the steps the host put on its control disc (the DC too). A
# step's `as` is "system" (the SYSTEM worker), "logon" (this machine's own logged-on user,
# foreground), or "user" (another lab user - a domain user in AD mode, a local user in
# workgroup mode - started by the worker with its password). Each LAB_STEP carries machine= so
# the host can tell the machines' reports apart.

# At run start in an AD lab, refresh group policy and pull any auto-enrollment certificates, so
# steps that expect a machine/user cert find one. Best-effort; works from Server 2012 R2 on.
function Update-LabPolicy($Cfg) {
    if ([string]$Cfg.mode -eq 'workgroup') { return }
    [void](Test-LabNative 'gpupdate.exe' @('/force'))
    # machine and (in this logged-on session) user auto-enrollment
    [void](Test-LabNative 'certutil.exe' @('-pulse'))
    [void](Test-LabNative 'certutil.exe' @('-user', '-pulse'))
    Send-WtlEvent 'LAB_POLICY' @('machine', [string]$Cfg.machine, 'state', 'refreshed')
}

function Invoke-LabMemberSteps($Cfg) {
    $run = $Cfg.run
    $machine = [string]$Cfg.machine
    if ($run -eq $null) { Send-WtlEvent 'LAB_RUN_DONE' @('machine', $machine, 'steps', 0) 'no run steps'; return 'done' }
    $steps = @($run.steps | Where-Object { $_ })
    $defWait = 60; if ($run.wait) { try { $defWait = [int]$run.wait } catch { } }

    Update-LabPolicy $Cfg
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
        # a domain user in an AD lab, a local account ('.') in a workgroup
        $req['domain'] = [string]$Cfg.netbios
        if ([string]$Cfg.mode -eq 'workgroup') { $req['domain'] = '.' }
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
    $role = [string]$Cfg.role
    if ($role -eq 'dc') {
        # the DC uses the same promotion path; Invoke-LabDc reads domain/netbios/users/ip/dc_ip
        $r = Invoke-LabDc $Cfg $LabDir
        if ($r -ne 'ready') { return $r }
        # the DC also needs the SYSTEM worker baked, so it can run its own steps
        Register-LabSvc $LabDir
        if ($Cfg.run -ne $null) { return (Invoke-LabMemberSteps $Cfg) }
        return 'ready'
    }
    if ($role -ne 'member') { throw "unknown role '$role' in lab.json" }
    # a member: join + autologon (phase 'new'), else it is logged on -> ready / run steps
    $r = Invoke-LabClient $Cfg $LabDir
    if ($r -ne 'ready') { return $r }
    # the 0-state build: the finished machine is frozen; the host powers it off via ACPI (a plain
    # domain user may not call Stop-Computer; ShutdownWithoutLogon covers it)
    if ($Cfg.shutdown_when_ready) { Send-WtlEvent 'LAB_IDLE'; return 'reboot' }
    if ($Cfg.run -ne $null) { return (Invoke-LabMemberSteps $Cfg) }
    return 'ready'
}

# ================================================================ entry

function Invoke-Lab([string]$LabDir) {
    try {
        $cfg = [System.IO.File]::ReadAllText((Join-Path $LabDir 'lab.json')) | ConvertFrom-Json
        Send-WtlEvent 'LAB_START' @('role', $cfg.role, 'phase', (Get-LabPhase), 'user', "$env:USERDOMAIN\$env:USERNAME")
        if ([string]$cfg.fmt -ne 'multi') { throw "lab.json fmt '$($cfg.fmt)' is not supported (this lab.ps1 reads fmt=multi; rebuild with the current lab)" }
        return (Invoke-LabMulti $cfg $LabDir)
    } catch {
        $msg = $_.Exception.Message
        try { $msg = "$msg (line $($_.InvocationInfo.ScriptLineNumber))" } catch { }
        Send-WtlEvent 'LAB_FAIL' @() $msg
        return 'fail'
    }
}
