# web.ps1 - the IIS default site on CA1 answers over the lab LAN. Run on CL1. Exit 0 on HTTP 200.
$ErrorActionPreference = 'Stop'
$url = 'http://CA1/'
try {
    $req = [System.Net.HttpWebRequest]::Create($url)
    $req.Timeout = 20000
    $resp = $req.GetResponse()
    $code = [int]$resp.StatusCode
    $resp.Close()
} catch [System.Net.WebException] {
    if ($_.Exception.Response) {
        $code = [int]([System.Net.HttpWebResponse]$_.Exception.Response).StatusCode
    } else {
        Write-Host ("GET $url failed: " + $_.Exception.Message)
        exit 1
    }
}
Write-Host ("GET $url -> HTTP $code")
if ($code -eq 200) { exit 0 }
exit 1
