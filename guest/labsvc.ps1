# labsvc.ps1 - win-test-lab privileged worker (guest side), runs as SYSTEM.
#
# PowerShell 4.0 compatible (Server 2012 R2 baseline). No PS5+ features.
# Baked into the 0 state: copied from the control disc to C:\WTL during the 0-state build and
# started at every boot by the scheduled task "WTL-Svc" (registered while still Administrator),
# running as SYSTEM.
#
# It does the things the logged-on domain user cannot: assign fixed drive letters to the mounted
# discs, run "system" steps with full privilege, and run steps as another lab user (with that
# user's password). It talks only to the filesystem, never the serial port (the logged-on
# user's agent owns COM1 and does the reporting), via a request/response queue under C:\WTL\svc:
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

    # run as another user (not logged on): SYSTEM logs that account on and starts the program
    # with its token; output still captured. No desktop.
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

# Start a program as another user from SYSTEM: LogonUser + CreateProcessAsUser (what services
# use; CreateProcessWithLogonW, behind Process.Start with credentials, refuses to run from
# LocalSystem). The user's profile is loaded and its environment used; stdin/stdout/stderr are
# files opened here and inherited, so the user needs no rights on C:\WTL. lpDesktop "" puts the
# process on a window station of its own logon session (no desktop; console programs and
# installers are fine).
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
        IntPtr h = write ? CreateFile(path, 0x40000000, 3, ref sa, 2, 0x80, IntPtr.Zero)   // GENERIC_WRITE, CREATE_ALWAYS
                         : CreateFile(path, 0x80000000, 3, ref sa, 3, 0x80, IntPtr.Zero);  // GENERIC_READ, OPEN_EXISTING
        if (h == new IntPtr(-1)) throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error(), "cannot open " + path);
        return h;
    }

    // Run CMDLINE as DOMAIN\USER (DOMAIN "." = a local account). Returns { state, code, pid }:
    // state 0 = exited with code, 1 = still running after timeoutMs (left running; the caller
    // stops it or keeps it).
    public static long[] Run(string user, string domain, string password, string cmdline, string cwd,
                             string outPath, string errPath, string inPath, int timeoutMs) {
        IntPtr token;
        // an interactive logon gives the usual user token; batch, then network, where the user
        // may not log on interactively (e.g. a plain domain user on a DC)
        if (!LogonUser(user, domain, password, 2, 0, out token) && !LogonUser(user, domain, password, 4, 0, out token)
            && !LogonUser(user, domain, password, 3, 0, out token))
            throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error(), "logon as " + domain + "\\" + user + " failed");
        PROFILEINFO prof = new PROFILEINFO();
        prof.dwSize = Marshal.SizeOf(typeof(PROFILEINFO));
        prof.dwFlags = 1;   // PI_NOUI
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
            si.dwFlags = 0x100;   // STARTF_USESTDHANDLES
            si.hStdInput = hIn; si.hStdOutput = hOut; si.hStdError = hErr;
            uint flags = 0x00000400 | 0x08000000;   // CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW
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

# Run a program as another lab user (not the logged-on one): a domain user in an AD lab
# ($Domain = the NetBIOS name), a local user in a workgroup lab ($Domain = '.').
function Invoke-SvcRunAs([string]$File, [string]$ArgLine, [string]$Cwd, [string]$OutFile, [string]$ErrFile,
                         [string]$User, [string]$Pass, [string]$Domain, [int]$Timeout, [bool]$KeepRunning) {
    if (-not ('WtlRunAs' -as [type])) { Add-Type -TypeDefinition $SvcRunAsSource }
    if (-not $Domain) { $Domain = '.' }
    $inFile = Join-Path $SvcDir 'empty.in'
    if (-not (Test-Path -LiteralPath $inFile)) { [System.IO.File]::WriteAllText($inFile, '') }
    $cmdline = '"' + $File + '"'
    if ($File -notmatch '\\') {
        # a bare program name (powershell.exe, msiexec.exe): CreateProcess finds it on the PATH
        $cmdline = $File
    }
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
