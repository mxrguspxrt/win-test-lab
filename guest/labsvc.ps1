# labsvc.ps1 - win-test-lab privileged worker (guest side), runs as SYSTEM.
#
# PowerShell 4.0 compatible (Server 2012 R2 baseline). No PS5+ features.
# Baked into the 0 state: copied to C:\WTL by build-image, started by the scheduled task
# "WTL-Svc" (created as admin during the client join phase) at every boot, running as SYSTEM.
#
# It does the things a limited LAB\student cannot: assign fixed drive letters to the mounted
# discs, and run "run_as_admin" steps with full privilege (SYSTEM = the lab's "root"). It talks
# only to the filesystem, never the serial port (the student agent owns COM1 and does the
# reporting), via a simple request/response queue under C:\WTL\svc:
#
#   request  C:\WTL\svc\<id>.req.json   {"action":"mount","letter":"X"}
#                                       {"action":"run","path":"X:\\a.bat","cwd":"X:\\","timeout":60,"out":"...","err":"..."}
#   result   C:\WTL\svc\<id>.res.json   {"ok":true,...} | {"ok":true,"exit":N,"secs":S} | {"ok":false,"error":".."}
#
# The worker writes <id>.res.json then deletes <id>.req.json. It loops forever.

$ErrorActionPreference = 'Stop'
$SvcDir = 'C:\WTL\svc'
$LogFile = 'C:\WTL\labsvc.log'

function Write-SvcLog([string]$Text) {
    try { [System.IO.File]::AppendAllText($LogFile, ((Get-Date).ToString('yyyy-MM-dd HH:mm:ss') + " $Text`r`n")) } catch { }
}

function Set-MountLetter([string]$Letter) {
    $label = 'WTL' + $Letter.ToUpper()
    $target = $Letter.ToUpper() + ':'
    $vol = Get-WmiObject -Class Win32_Volume -Filter "Label='$label'" -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $vol) { throw "no disc labelled $label" }
    if ($vol.DriveLetter -ne $target) {
        $clash = Get-WmiObject -Class Win32_Volume -Filter "DriveLetter='$target'" -ErrorAction SilentlyContinue | Select-Object -First 1
        if ($clash) { $clash.DriveLetter = $null; [void]$clash.Put() }
        $vol.DriveLetter = $target
        [void]$vol.Put()
    }
    return $target
}

# Build the (file, argLine) a script path turns into: .ps1 via powershell, .bat/.cmd via cmd,
# .msi via msiexec, anything else run directly. $UserArgs are the step's own arguments.
function Resolve-SvcLaunch([string]$Path, [string]$UserArgs) {
    $ext = [System.IO.Path]::GetExtension($Path).ToLowerInvariant()
    $file = $Path; $argLine = ''
    if ($ext -eq '.ps1') {
        $file = 'powershell.exe'; $argLine = '-NoProfile -ExecutionPolicy Bypass -File "' + $Path + '"'
        if ($UserArgs) { $argLine += ' ' + $UserArgs }
    } elseif ($ext -eq '.bat' -or $ext -eq '.cmd') {
        $file = $env:ComSpec; $argLine = '/d /c ""' + $Path + '"'
        if ($UserArgs) { $argLine += ' ' + $UserArgs }
        $argLine += '"'
    } elseif ($ext -eq '.msi') {
        $file = 'msiexec.exe'; $argLine = '/i "' + $Path + '" /qn /norestart'
        if ($UserArgs) { $argLine += ' ' + $UserArgs }
    } elseif ($UserArgs) {
        $argLine = $UserArgs
    }
    return @($file, $argLine)
}

function Invoke-SvcRun($Req) {
    $path = [string]$Req.path
    $cwd = [string]$Req.cwd
    $timeout = 60; if ($Req.timeout) { try { $timeout = [int]$Req.timeout } catch { } }
    $outFile = [string]$Req.out
    $errFile = [string]$Req.err
    $userArgs = [string]$Req.args
    $runUser = [string]$Req.user          # run as this account instead of SYSTEM (optional)
    $runPass = [string]$Req.password
    $runDomain = [string]$Req.domain
    $keepRunning = $false; if ($Req.keep_running) { $keepRunning = [bool]$Req.keep_running }
    $inFile = Join-Path $SvcDir 'empty.in'
    foreach ($f in @($outFile, $errFile)) { if ($f -and (Test-Path $f)) { Remove-Item $f -Force } }
    [System.IO.File]::WriteAllText($inFile, '')

    $resolved = Resolve-SvcLaunch $path $userArgs
    $file = $resolved[0]; $argLine = $resolved[1]

    # run as another user (not logged on): SYSTEM launches it with that account's credentials
    # via ProcessStartInfo (CreateProcessWithLogonW); output still captured. No GUI (session 0).
    if ($runUser) {
        return (Invoke-SvcRunAs $file $argLine $cwd $outFile $errFile $runUser $runPass $runDomain $timeout $keepRunning)
    }

    $sp = @{ FilePath = $file; PassThru = $true; NoNewWindow = $true;
             RedirectStandardOutput = $outFile; RedirectStandardError = $errFile; RedirectStandardInput = $inFile }
    if ($cwd -and (Test-Path -LiteralPath $cwd)) { $sp['WorkingDirectory'] = $cwd }
    if ($argLine) { $sp['ArgumentList'] = $argLine }
    $t0 = [DateTime]::Now
    $proc = Start-Process @sp
    try { $null = $proc.Handle } catch { }
    $deadline = $t0.AddSeconds([Math]::Max(5, $timeout))
    $timedOut = $false
    while (-not $proc.HasExited) {
        if ([DateTime]::Now -ge $deadline) { $timedOut = $true; break }
        Start-Sleep -Milliseconds 250
    }
    if ($timedOut) {
        if ($keepRunning) {
            return @{ ok = $true; running = $true; secs = [Math]::Round(([DateTime]::Now - $t0).TotalSeconds, 1) }
        }
        try { & taskkill.exe /T /F /PID $proc.Id 2>&1 | Out-Null } catch { }
        try { if (-not $proc.HasExited) { $proc.Kill() } } catch { }
        return @{ ok = $true; timed_out = $true; secs = [Math]::Round(([DateTime]::Now - $t0).TotalSeconds, 1) }
    }
    $code = -1
    try { $proc.WaitForExit(); $code = $proc.ExitCode } catch { }
    return @{ ok = $true; exit = $code; secs = [Math]::Round(([DateTime]::Now - $t0).TotalSeconds, 1) }
}

# Run a program as another domain user (not the logged-on one). SYSTEM has SeAssignPrimaryToken,
# so ProcessStartInfo with UserName/Password uses CreateProcessWithLogonW. Output is redirected
# to files. The process runs in session 0 with no desktop (fine for console programs / installers).
function Invoke-SvcRunAs([string]$File, [string]$ArgLine, [string]$Cwd, [string]$OutFile, [string]$ErrFile,
                         [string]$User, [string]$Pass, [string]$Domain, [int]$Timeout, [bool]$KeepRunning) {
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $File
    $psi.Arguments = $ArgLine
    if ($Cwd -and (Test-Path -LiteralPath $Cwd)) { $psi.WorkingDirectory = $Cwd }
    $psi.UserName = $User
    if ($Domain) { $psi.Domain = $Domain }
    $sec = New-Object System.Security.SecureString
    foreach ($ch in $Pass.ToCharArray()) { $sec.AppendChar($ch) }
    $psi.Password = $sec
    $psi.UseShellExecute = $false
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.CreateNoWindow = $true
    $t0 = [DateTime]::Now
    $proc = $null
    try { $proc = [System.Diagnostics.Process]::Start($psi) } catch {
        return @{ ok = $false; error = "run as ${Domain}\${User} failed: $($_.Exception.Message)" }
    }
    if ($proc -eq $null) { return @{ ok = $false; error = "run as ${Domain}\${User}: Process.Start returned null" } }
    # drain the pipes on background threads so a chatty program cannot deadlock
    $outBuf = New-Object System.Text.StringBuilder
    $errBuf = New-Object System.Text.StringBuilder
    $null = Register-ObjectEvent -InputObject $proc -EventName OutputDataReceived -Action { if ($EventArgs.Data -ne $null) { [void]$Event.MessageData.Append($EventArgs.Data + "`r`n") } } -MessageData $outBuf
    $null = Register-ObjectEvent -InputObject $proc -EventName ErrorDataReceived -Action { if ($EventArgs.Data -ne $null) { [void]$Event.MessageData.Append($EventArgs.Data + "`r`n") } } -MessageData $errBuf
    $proc.BeginOutputReadLine()
    $proc.BeginErrorReadLine()
    $deadline = $t0.AddSeconds([Math]::Max(5, $Timeout))
    $timedOut = $false
    while (-not $proc.HasExited) {
        if ([DateTime]::Now -ge $deadline) { $timedOut = $true; break }
        Start-Sleep -Milliseconds 250
    }
    $flush = {
        try { [System.IO.File]::WriteAllText($OutFile, $outBuf.ToString()) } catch { }
        try { [System.IO.File]::WriteAllText($ErrFile, $errBuf.ToString()) } catch { }
    }
    if ($timedOut -and $KeepRunning) {
        & $flush
        return @{ ok = $true; running = $true; secs = [Math]::Round(([DateTime]::Now - $t0).TotalSeconds, 1) }
    }
    if ($timedOut) {
        try { & taskkill.exe /T /F /PID $proc.Id 2>&1 | Out-Null } catch { }
        try { if (-not $proc.HasExited) { $proc.Kill() } } catch { }
        & $flush
        return @{ ok = $true; timed_out = $true; secs = [Math]::Round(([DateTime]::Now - $t0).TotalSeconds, 1) }
    }
    $code = -1
    try { $proc.WaitForExit(); $code = $proc.ExitCode } catch { }
    Start-Sleep -Milliseconds 100
    & $flush
    return @{ ok = $true; exit = $code; secs = [Math]::Round(([DateTime]::Now - $t0).TotalSeconds, 1) }
}

function Invoke-Request($ReqPath) {
    $res = $null
    try {
        $req = [System.IO.File]::ReadAllText($ReqPath) | ConvertFrom-Json
        switch ([string]$req.action) {
            'mount' { $t = Set-MountLetter ([string]$req.letter); $res = @{ ok = $true; letter = $t } }
            'run'   { $res = Invoke-SvcRun $req }
            default { $res = @{ ok = $false; error = "unknown action '$($req.action)'" } }
        }
    } catch {
        $res = @{ ok = $false; error = $_.Exception.Message }
    }
    $resPath = $ReqPath -replace '\.req\.json$', '.res.json'
    try { [System.IO.File]::WriteAllText($resPath, ($res | ConvertTo-Json -Compress)) } catch { Write-SvcLog "write result failed: $($_.Exception.Message)" }
    try { Remove-Item -LiteralPath $ReqPath -Force } catch { }
    Write-SvcLog "handled $(Split-Path -Leaf $ReqPath) -> ok=$($res.ok)"
}

# single instance
$mutex = New-Object System.Threading.Mutex($false, 'Global\WTL-Svc')
try { if (-not $mutex.WaitOne(0)) { exit 0 } } catch [System.Threading.AbandonedMutexException] { }

if (-not (Test-Path $SvcDir)) { New-Item -ItemType Directory -Path $SvcDir -Force | Out-Null }
Write-SvcLog "labsvc started as $([Security.Principal.WindowsIdentity]::GetCurrent().Name)"
while ($true) {
    try {
        $reqs = Get-ChildItem -LiteralPath $SvcDir -Filter '*.req.json' -ErrorAction SilentlyContinue | Sort-Object Name
        foreach ($r in $reqs) { Invoke-Request $r.FullName }
    } catch { Write-SvcLog "loop error: $($_.Exception.Message)" }
    Start-Sleep -Milliseconds 500
}
