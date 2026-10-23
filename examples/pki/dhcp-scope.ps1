# dhcp-scope.ps1 - the DHCP server on DC1 has the lab scope and is authorized. Run on DC1 as
# SYSTEM. Exit 0 if a scope is present.
$ErrorActionPreference = 'Stop'
Import-Module DhcpServer -ErrorAction SilentlyContinue
$scopes = @(Get-DhcpServerv4Scope -ErrorAction SilentlyContinue)
if ($scopes.Count -eq 0) {
    Write-Host "no IPv4 DHCP scope on this server"
    exit 1
}
foreach ($s in $scopes) { Write-Host ("scope " + $s.ScopeId + "  " + $s.StartRange + " - " + $s.EndRange + "  (" + $s.State + ")") }
# the authorization list (best effort; empty in a lab that lost its DC link is still informative)
try {
    $auth = @(Get-DhcpServerInDC -ErrorAction SilentlyContinue)
    foreach ($a in $auth) { Write-Host ("authorized in AD: " + $a.DnsName + "  " + $a.IPAddress) }
} catch { }
exit 0
