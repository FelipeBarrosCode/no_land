[CmdletBinding(DefaultParameterSetName = 'Directory')]
param(
    [Parameter(Mandatory = $true, ParameterSetName = 'Directory')][string]$Directory,
    [Parameter(Mandatory = $true, ParameterSetName = 'File')][string]$FilePath,
    [switch]$Recurse
)

$ErrorActionPreference = 'Stop'
$files = if ($PSCmdlet.ParameterSetName -eq 'File') {
    @(Get-Item -LiteralPath $FilePath)
} else {
    @(Get-ChildItem -LiteralPath $Directory -File -Recurse:$Recurse |
        Where-Object { $_.Extension -in @('.exe', '.msi') })
}
if ($files.Count -eq 0) { throw "No Windows signing inputs found in $Directory" }
foreach ($file in $files) {
    $signature = Get-AuthenticodeSignature -LiteralPath $file.FullName
    if ($signature.Status -ne 'Valid') {
        throw "Invalid Authenticode signature on $($file.FullName): $($signature.Status)"
    }
    if ($null -eq $signature.SignerCertificate -or $null -eq $signature.TimeStamperCertificate) {
        throw "Missing signer certificate or trusted timestamp on $($file.FullName)"
    }
    Write-Output "Verified $($file.Name): signer=$($signature.SignerCertificate.Subject); thumbprint=$($signature.SignerCertificate.Thumbprint); timestamp=$($signature.TimeStamperCertificate.Subject)"
}
