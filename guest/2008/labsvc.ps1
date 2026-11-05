# labsvc.ps1 (2008/2003) - win-test-lab privileged worker (guest side), runs as SYSTEM.
#
# PowerShell 2.0 / .NET 2.0 compatible: what Server 2008 R2 and 2003 ship. Same request/response
# contract and behaviour as guest/labsvc.ps1, but with an in-house JSON parser/emitter (PS 2.0
# has no ConvertFrom-Json / ConvertTo-Json) and no arm64 virtio-serial path (these OSes are x64
# with a real COM1, so the logged-on agent writes the serial line itself).
#
# Baked into the 0 state and started at every boot by the scheduled task "WTL-Svc" (SYSTEM).
# Talks only to the filesystem, via a request/response queue under C:\WTL\svc:
#   request  C:\WTL\svc\<id>.req.json   {"action":"mount","letter":"X"}
#                                       {"action":"run","path":"X:\\a.bat","cwd":"X:\\","timeout":60,"out":"...","err":"..."}
#   result   C:\WTL\svc\<id>.res.json   {"ok":true,...} | {"ok":true,"exit":N,"secs":S} | {"ok":false,"error":".."}

$ErrorActionPreference = 'Stop'
$SvcDir = 'C:\WTL\svc'
$LogFile = 'C:\WTL\labsvc.log'

function Write-SvcLog([string]$Text) {
    try { [System.IO.File]::AppendAllText($LogFile, ((Get-Date).ToString('yyyy-MM-dd HH:mm:ss') + " $Text`r`n")) } catch { }
}

# ---------------------------------------------------------------- JSON (PowerShell 2.0 has none)

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
    $script:JsonPos++
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
            default { [void]$sb.Append($e) }
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
            [void]$list.Add((Read-WtlJsonValue))
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

# ---------------------------------------------------------------- actions

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

# Start a program as another user from SYSTEM: LogonUser + CreateProcessAsUser.
$SvcRunAsSource = @'
using System;
using System.Runtime.InteropServices;
public static class WtlRunAs {
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    struct STARTUPINFO {
        public int cb; public string lpReserved; public string lpDesktop; public string lpTitle;
        public int dwX, dwY, dwXSize, dwYSize, dwXCountChars, dwYCountChars, dwFillAttribute, dwFlags;
        public short wShowWindow, cbReserved2; public IntPtr lpReserved2, hStdInput, hStdOutput, hStdError;
    }
    [StructLayout(LayoutKind.Sequential)]
    struct PROCESS_INFORMATION { public IntPtr hProcess, hThread; public int dwProcessId, dwThreadId; }
    [StructLayout(LayoutKind.Sequential)]
    struct SECURITY_ATTRIBUTES { public int nLength; public IntPtr lpSecurityDescriptor; public bool bInheritHandle; }
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    struct PROFILEINFO {
        public int dwSize, dwFlags; public string lpUserName, lpProfilePath, lpDefaultPath, lpServerName, lpPolicyPath;
        public IntPtr hProfile;
    }
    [DllImport("advapi32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    static extern bool LogonUser(string user, string domain, string password, int type, int provider, out IntPtr token);
    [DllImport("advapi32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    static extern bool CreateProcessAsUser(IntPtr token, string app, string cmd, IntPtr pa, IntPtr ta, bool inherit,
        uint flags, IntPtr env, string cwd, ref STARTUPINFO si, out PROCESS_INFORMATION pi);
    [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    static extern IntPtr CreateFile(string name, uint access, uint share, ref SECURITY_ATTRIBUTES sa, uint disposition, uint flags, IntPtr template);
    [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr h);
    [DllImport("kernel32.dll")] static extern uint WaitForSingleObject(IntPtr h, uint ms);
    [DllImport("kernel32.dll")] static extern bool GetExitCodeProcess(IntPtr h, out uint code);
    [DllImport("userenv.dll", SetLastError = true, CharSet = CharSet.Unicode)] static extern bool LoadUserProfile(IntPtr token, ref PROFILEINFO pi);
    [DllImport("userenv.dll", SetLastError = true)] static extern bool UnloadUserProfile(IntPtr token, IntPtr profile);
    [DllImport("userenv.dll", SetLastError = true)] static extern bool CreateEnvironmentBlock(out IntPtr env, IntPtr token, bool inherit);
    [DllImport("userenv.dll", SetLastError = true)] static extern bool DestroyEnvironmentBlock(IntPtr env);

    static IntPtr Open(string path, bool write) {
        SECURITY_ATTRIBUTES sa = new SECURITY_ATTRIBUTES();
        sa.nLength = Marshal.SizeOf(typeof(SECURITY_ATTRIBUTES));
        sa.bInheritHandle = true;
        IntPtr h = write ? CreateFile(path, 0x40000000, 3, ref sa, 2, 0x80, IntPtr.Zero)
                         : CreateFile(path, 0x80000000, 3, ref sa, 3, 0x80, IntPtr.Zero);
        if (h == new IntPtr(-1)) throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error(), "cannot open " + path);
        return h;
    }

    public static long[] Run(string user, string domain, string password, string cmdline, string cwd,
                             string outPath, string errPath, string inPath, int timeoutMs) {
        IntPtr token;
        if (!LogonUser(user, domain, password, 2, 0, out token) && !LogonUser(user, domain, password, 4, 0, out token)
            && !LogonUser(user, domain, password, 3, 0, out token))
            throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error(), "logon as " + domain + "\\" + user + " failed");
        PROFILEINFO prof = new PROFILEINFO();
        prof.dwSize = Marshal.SizeOf(typeof(PROFILEINFO));
        prof.dwFlags = 1;
        prof.lpUserName = user;
        bool profLoaded = LoadUserProfile(token, ref prof);
        IntPtr env;
        if (!CreateEnvironmentBlock(out env, token, false)) env = IntPtr.Zero;
        IntPtr hIn = Open(inPath, false), hOut = Open(outPath, true), hErr = Open(errPath, true);
        PROCESS_INFORMATION pi;
        try {
            STARTUPINFO si = new STARTUPINFO();
            si.cb = Marshal.SizeOf(typeof(STARTUPINFO));
            si.lpDesktop = "";
            si.dwFlags = 0x100;
            si.hStdInput = hIn; si.hStdOutput = hOut; si.hStdError = hErr;
            uint flags = 0x00000400 | 0x08000000;
            if (!CreateProcessAsUser(token, null, cmdline, IntPtr.Zero, IntPtr.Zero, true, flags, env, cwd, ref si, out pi))
                throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error(), "starting " + cmdline + " as " + domain + "\\" + user + " failed");
        } finally {
            CloseHandle(hIn); CloseHandle(hOut); CloseHandle(hErr);
            if (env != IntPtr.Zero) DestroyEnvironmentBlock(env);
        }
        CloseHandle(pi.hThread);
        long state = 1, code = 0;
        if (WaitForSingleObject(pi.hProcess, (uint)Math.Max(0, timeoutMs)) == 0) {
            uint c;
            GetExitCodeProcess(pi.hProcess, out c);
            state = 0; code = (int)c;
        }
        CloseHandle(pi.hProcess);
        if (state == 0 && profLoaded) UnloadUserProfile(token, prof.hProfile);
        CloseHandle(token);
        return new long[] { state, code, pi.dwProcessId };
    }
}
'@

function Invoke-SvcRunAs([string]$File, [string]$ArgLine, [string]$Cwd, [string]$OutFile, [string]$ErrFile,
                         [string]$User, [string]$Pass, [string]$Domain, [int]$Timeout, [bool]$KeepRunning) {
    if (-not ('WtlRunAs' -as [type])) { Add-Type -TypeDefinition $SvcRunAsSource }
    if (-not $Domain) { $Domain = '.' }
    $inFile = Join-Path $SvcDir 'empty.in'
    if (-not (Test-Path -LiteralPath $inFile)) { [System.IO.File]::WriteAllText($inFile, '') }
    $cmdline = '"' + $File + '"'
    if ($File -notmatch '\\') { $cmdline = $File }
    if ($ArgLine) { $cmdline += ' ' + $ArgLine }
    $wd = $null
    if ($Cwd -and (Test-Path -LiteralPath $Cwd)) { $wd = $Cwd }
    $t0 = [DateTime]::Now
    $r = $null
    try { $r = [WtlRunAs]::Run($User, $Domain, $Pass, $cmdline, $wd, $OutFile, $ErrFile, $inFile, [Math]::Max(5, $Timeout) * 1000) } catch {
        return @{ ok = $false; error = "run as ${Domain}\${User} failed: $($_.Exception.Message)" }
    }
    $secs = [Math]::Round(([DateTime]::Now - $t0).TotalSeconds, 1)
    if ($r[0] -eq 1) {
        if ($KeepRunning) { return @{ ok = $true; running = $true; secs = $secs } }
        try { & taskkill.exe /T /F /PID $r[2] 2>&1 | Out-Null } catch { }
        return @{ ok = $true; timed_out = $true; secs = $secs }
    }
    return @{ ok = $true; exit = [int]$r[1]; secs = $secs }
}

function Invoke-Request($ReqPath) {
    $res = $null
    try {
        $req = ConvertFrom-WtlJson ([System.IO.File]::ReadAllText($ReqPath))
        switch ([string]$req.action) {
            'mount' { $t = Set-MountLetter ([string]$req.letter); $res = @{ ok = $true; letter = $t } }
            'run'   { $res = Invoke-SvcRun $req }
            default { $res = @{ ok = $false; error = "unknown action '$($req.action)'" } }
        }
    } catch {
        $res = @{ ok = $false; error = $_.Exception.Message }
    }
    $resPath = $ReqPath -replace '\.req\.json$', '.res.json'
    try { [System.IO.File]::WriteAllText($resPath, (ConvertTo-WtlJson $res)) } catch { Write-SvcLog "write result failed: $($_.Exception.Message)" }
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
    Start-Sleep -Milliseconds 200
}
