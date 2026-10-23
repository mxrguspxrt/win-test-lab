# computer-cert.ps1 - a computer certificate was auto-enrolled into the local machine store,
# issued by the lab's Enterprise Root CA. Run on CL1 as SYSTEM. Exit 0 if found.
$ErrorActionPreference = 'Stop'
$certs = @(Get-ChildItem Cert:\LocalMachine\My -ErrorAction SilentlyContinue | Where-Object { $_.Issuer -match 'PKI-ROOT-CA' })
if ($certs.Count -eq 0) {
    Write-Host "no computer certificate issued by PKI-ROOT-CA in Cert:\LocalMachine\My"
    Get-ChildItem Cert:\LocalMachine\My | Format-Table Subject, Issuer -AutoSize | Out-String | Write-Host
    exit 1
}
foreach ($c in $certs) { Write-Host ("computer cert: " + $c.Subject + "  <- " + $c.Issuer + "  (thumbprint " + $c.Thumbprint + ")") }
exit 0
