$ErrorActionPreference = 'SilentlyContinue'

Write-Host 'Stopping Proton VPN processes...'
Get-Process | Where-Object {
    $_.ProcessName -like 'Proton*' -or
    $_.ProcessName -like '*ProtonVPN*'
} | Stop-Process -Force

Write-Host 'Stopping Proton VPN services...'
Get-Service | Where-Object {
    $_.Name -like 'Proton*' -or
    $_.DisplayName -like '*Proton*'
} | Stop-Service -Force

Write-Host 'Disabling Proton tunnel adapter...'
netsh interface set interface name="ProTUN" admin=disabled | Out-Host

Write-Host 'Removing accidental default routes through Noland tunnel...'
Get-NetRoute -AddressFamily IPv4 -DestinationPrefix '0.0.0.0/0' | Where-Object {
    $_.InterfaceAlias -eq 'nolandwg0' -or $_.InterfaceAlias -eq 'ProTUN'
} | Remove-NetRoute -Confirm:$false

Write-Host 'Remaining VPN/Noland adapters:'
Get-NetAdapter -Name 'ProTUN','nolandwg0' -ErrorAction SilentlyContinue | Select-Object Name,Status,ifIndex,InterfaceDescription | Format-Table -AutoSize

Write-Host 'Remaining default routes:'
Get-NetRoute -AddressFamily IPv4 -DestinationPrefix '0.0.0.0/0' | Select-Object DestinationPrefix,InterfaceAlias,NextHop,RouteMetric,ifIndex | Format-Table -AutoSize
