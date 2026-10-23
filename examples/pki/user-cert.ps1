# user-cert.ps1 - a user certificate was auto-enrolled into the current user's store, issued by
# the lab's Enterprise Root CA. Run on CL1 as the logged-on domain user. Exit 0 if found.
# Enrollment happens in the background after a policy refresh: poll for up to 3 minutes.
$ErrorActionPreference = 'Stop'
$deadline = (Get-Date).AddMinutes(3)
while ($true) {
    $certs = @(Get-ChildItem Cert:\CurrentUser\My -ErrorAction SilentlyContinue | Where-Object { $_.Issuer -match 'PKI-ROOT-CA' })
    if ($certs.Count -gt 0) { break }
    if ((Get-Date) -ge $deadline) {
        Write-Host "no user certificate issued by PKI-ROOT-CA in Cert:\CurrentUser\My"
        Get-ChildItem Cert:\CurrentUser\My | Format-Table Subject, Issuer -AutoSize | Out-String | Write-Host
        exit 1
    }
    & certutil.exe -user -pulse | Out-Null
    Start-Sleep -Seconds 10
}
foreach ($c in $certs) { Write-Host ("user cert: " + $c.Subject + "  <- " + $c.Issuer + "  (thumbprint " + $c.Thumbprint + ")") }
exit 0
