# echo-server.ps1 - win-test-lab demo "program" (PowerShell 4.0 / .NET 4.5 compatible)
#
# A TCP line-chat server on port 5000 (override: first argument).
# Protocol (one line per command, CRLF or LF):
#   on connect   -> "HELLO wtl-demo os=<OS caption>"
#   PING         -> "PONG"
#   ECHO <text>  -> "<text>"
#   ADD <a> <b>  -> "<a+b>"
#   BYE          -> "BYE" and the connection is closed
#   anything else-> "ERR unknown command"
# Clients are served one after another; the server loops forever.

param([int]$Port = 5000)

$ErrorActionPreference = 'Continue'

try {
    $caption = (Get-WmiObject -Class Win32_OperatingSystem).Caption
} catch {
    $caption = [Environment]::OSVersion.VersionString
}
if (-not $caption) { $caption = 'unknown' }
$caption = $caption.Trim()

function Get-Reply([string]$line) {
    $line = $line.Trim()
    if ($line -eq 'PING') { return 'PONG' }
    if ($line -eq 'BYE') { return 'BYE' }
    if ($line -eq 'ECHO') { return '' }
    if ($line.StartsWith('ECHO ')) { return $line.Substring(5) }
    if ($line.StartsWith('ADD ')) {
        $parts = $line.Substring(4).Trim() -split '\s+'
        if ($parts.Count -eq 2) {
            [long]$a = 0; [long]$b = 0
            if ([long]::TryParse($parts[0], [ref]$a) -and [long]::TryParse($parts[1], [ref]$b)) {
                return [string]($a + $b)
            }
        }
        return 'ERR bad numbers'
    }
    return 'ERR unknown command'
}

$listener = New-Object System.Net.Sockets.TcpListener([System.Net.IPAddress]::Any, $Port)
$listener.Start()
Write-Host "wtl-demo listening on 0.0.0.0:$Port (os=$caption)"

while ($true) {
    $client = $null
    try {
        $client = $listener.AcceptTcpClient()
        $client.NoDelay = $true
        $stream = $client.GetStream()
        $enc = New-Object System.Text.UTF8Encoding($false)
        $reader = New-Object System.IO.StreamReader($stream, $enc)
        $writer = New-Object System.IO.StreamWriter($stream, $enc)
        $writer.NewLine = "`r`n"
        $writer.AutoFlush = $true
        Write-Host "client connected: $($client.Client.RemoteEndPoint)"
        $writer.WriteLine("HELLO wtl-demo os=$caption")
        while ($true) {
            $line = $reader.ReadLine()
            if ($line -eq $null) { break }
            $reply = Get-Reply $line
            $writer.WriteLine($reply)
            if ($reply -eq 'BYE') { break }
        }
    } catch {
        Write-Host "client error: $($_.Exception.Message)"
    } finally {
        if ($client -ne $null) { try { $client.Close() } catch { } }
    }
}
