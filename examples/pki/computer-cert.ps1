# computer-cert.ps1 - a computer certificate for this computer's name was auto-enrolled into the
# local machine store, issued by the lab's Enterprise Root CA. Run on CL1 as SYSTEM. Exit 0 if
# found. (A renamed computer must have one under its new name.)
# Enrollment happens in the background after a policy refresh: poll for up to 3 minutes.
$ErrorActionPreference = 'Stop'
$deadline = (Get-Date).AddMinutes(3)
while ($true) {
    $certs = @(Get-ChildItem Cert:\LocalMachine\My -ErrorAction SilentlyContinue | Where-Object {
            $_.Issuer -match 'PKI-ROOT-CA' -and (@($_.DnsNameList | ForEach-Object { [string]$_ }) -match ('^' + [regex]::Escape($env:COMPUTERNAME) + '(\.|$)'))
        })
    if ($certs.Count -gt 0) { break }
    if ((Get-Date) -ge $deadline) {
        Write-Host "no computer certificate for $env:COMPUTERNAME issued by PKI-ROOT-CA in Cert:\LocalMachine\My"
        Get-ChildItem Cert:\LocalMachine\My | Format-Table Subject, Issuer -AutoSize | Out-String | Write-Host
        exit 1
    }
    & certutil.exe -pulse | Out-Null
    Start-Sleep -Seconds 10
}
foreach ($c in $certs) { Write-Host ("computer cert: " + $c.Subject + "  <- " + $c.Issuer + "  (thumbprint " + $c.Thumbprint + ")") }
exit 0
