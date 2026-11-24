# rename.ps1 - win-test-lab: the privileged half of renaming a machine of an environment (guest
# side), run as SYSTEM by the worker when lab.ps1 asks (see Invoke-LabRename there). It ships on
# the control disc next to lab.ps1, so a 0 state built by an older wtl has it too, and it loads
# lab.ps1 to use the same functions as the build.
#
#   rename.ps1 -Mode rename -LabDir D:\LAB   rename this computer to lab.json's rename.to, then
#       reboot in 5 s. A CA host's CA is uninstalled first; a workgroup machine's automatic logon
#       follows the new name (its local accounts are COMPUTER\user); a member that needs the
#       domain Administrator after the rename (rename.admin) logs on as it next (phase
#       rename-admin).
#   rename.ps1 -Mode finish -LabDir D:\LAB   after the rename: the IIS page names the new name,
#       the machine's certificates are requested again (rename.reenroll).
#
# PowerShell 4.0 compatible. Exit 0 = done; else the error is on stderr.
param([string]$Mode, [string]$LabDir)
$ErrorActionPreference = 'Stop'
$RenameLog = 'C:\WTL\run\rename.log'

# lab.ps1's helpers report over the agent's serial line; here (SYSTEM, no COM1) they go to a log
function Send-WtlEvent([string]$EventName, [object[]]$Pairs = @(), [string]$Text = '') {
    Add-Content -Path $RenameLog -Value ("{0:s} {1} {2} {3}" -f (Get-Date), $EventName, ($Pairs -join ' '), $Text)
}
function Send-LabPhase([string]$Text) { Send-WtlEvent 'LAB_PHASE' @() $Text }

try {
    . (Join-Path $LabDir 'lab.ps1')
    $Cfg = [System.IO.File]::ReadAllText((Join-Path $LabDir 'lab.json')) | ConvertFrom-Json
    $rn = $Cfg.rename
    if ($rn -eq $null) { throw 'lab.json has no rename' }
    $to = [string]$rn.to
    $ad = ([string]$Cfg.mode -ne 'workgroup')
    if ($Mode -eq 'rename') {
        Set-LabShutdownFriendly
        if ($rn.ca) { Uninstall-LabCa }
        $r = @{ NewName = $to; Force = $true; ErrorAction = 'Stop'; WarningAction = 'SilentlyContinue' }
        if ($ad) {
            $r['DomainCredential'] = New-Object System.Management.Automation.PSCredential(([string]$Cfg.netbios + '\Administrator'),
                (ConvertTo-LabSecure ([string]$Cfg.admin_password)))
        }
        # a DC that just took its new name may need a moment before it renames a member
        for ($i = 1; ; $i++) {
            try { Rename-Computer @r; break } catch {
                if ($i -ge 20) { throw "Rename-Computer $to failed: $($_.Exception.Message)" }
                Send-LabPhase "rename attempt $i failed: $($_.Exception.Message)"
                Start-Sleep -Seconds 15
            }
        }
        $user = [string]$Cfg.logon_user; $pass = [string]$Cfg.logon_password
        if (-not $user) { $user = 'Administrator'; $pass = [string]$Cfg.admin_password }
        if (-not $ad) {
            # local accounts are COMPUTER\user: the automatic logon follows the new name
            Set-LabAutoLogon $to $user $pass
        } elseif ($rn.admin) {
            # after the rename: the CA (or DHCP) needs the domain Administrator's session
            Set-LabAutoLogon ([string]$Cfg.netbios) 'Administrator' ([string]$Cfg.admin_password)
            Set-LabPhase 'rename-admin'
        }
        & shutdown.exe /r /t 5 /f | Out-Null
    } elseif ($Mode -eq 'finish') {
        if ([string]$rn.iis_site_path) { Write-LabIisPage ([string]$rn.iis_site_path) }
        if ($rn.reenroll) {
            Remove-LabCertsFrom 'LocalMachine' ([string]$rn.ca_name)
            & certutil.exe -pulse | Out-Null
        }
    } else {
        throw "unknown -Mode '$Mode' (rename or finish)"
    }
    exit 0
} catch {
    [Console]::Error.WriteLine($_.Exception.Message)
    Send-WtlEvent 'RENAME_FAIL' @() $_.Exception.Message
    exit 1
}
