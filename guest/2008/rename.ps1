# rename.ps1 (2008) - win-test-lab: the privileged half of renaming a Windows Server 2008 R2
# machine of an environment (guest side), run as SYSTEM by the worker when lab.ps1 asks (see
# Invoke-LabRename there). It ships on the control disc next to lab.ps1, so a 0 state built by an
# older wtl has it too, and it loads lab.ps1 to use the build's own functions.
#
#   rename.ps1 -LabDir D:\LAB   rename this computer to lab.json's rename.to (WMI: PowerShell 2.0
#       has no Rename-Computer), point the automatic logon (COMPUTER\user) at the new name, reset
#       the evaluation's activation timer, reboot in 5 s. Exit 0 = done; else the error is on
#       stderr.
#
# The activation timer: Microsoft's 2008 R2 Test Drive image runs on an expired evaluation grace
# period ("notification" mode). Renaming makes Windows check its license again, and the next
# logon then stops at a blocking "the activation period has expired" dialog that the automatic
# logon cannot pass. slmgr /rearm (the image has a few) restarts the grace period for this copy.
param([string]$LabDir)
$ErrorActionPreference = 'Stop'
$RenameLog = 'C:\WTL\run\rename.log'

# lab.ps1's helpers report over the agent's serial line; here (SYSTEM, no COM1) they go to a log
function Send-WtlEvent([string]$EventName, [object[]]$Pairs = @(), [string]$Text = '') {
    Add-Content -Path $RenameLog -Value ("{0:s} {1} {2} {3}" -f (Get-Date), $EventName, ($Pairs -join ' '), $Text)
}

try {
    . (Join-Path $LabDir 'lab.ps1')
    $Cfg = ConvertFrom-WtlJson ([System.IO.File]::ReadAllText((Join-Path $LabDir 'lab.json')))
    $to = [string]$Cfg.rename.to
    if (-not $to) { throw 'lab.json has no rename.to' }
    Rename-LabComputer $to
    $user = [string]$Cfg.logon_user; $pass = [string]$Cfg.logon_password
    if (-not $user) { $user = 'Administrator'; $pass = [string]$Cfg.admin_password }
    Set-LabAutoLogon $to $user $pass
    $rearm = & cscript.exe //nologo (Join-Path $env:SystemRoot 'System32\slmgr.vbs') /rearm 2>&1 | Out-String
    Send-WtlEvent 'LAB_PHASE' @() ("slmgr /rearm: " + $rearm.Trim())
    & shutdown.exe /r /t 5 /f | Out-Null
    exit 0
} catch {
    [Console]::Error.WriteLine($_.Exception.Message)
    Send-WtlEvent 'RENAME_FAIL' @() $_.Exception.Message
    exit 1
}
