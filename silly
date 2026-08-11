Silly

Most likely issue from your error

This part is the smoking gun:

> `netsh interface ipv4 set address name="nolandwg0" ...`
> `The object already exists.`

That usually means Windows already has stale state on the adapter:
- stale IP assignment
- stale route
- stale old helper run
- stale hidden Wintun adapter state

---

## Best immediate cleanup to try

In **PowerShell as Administrator**:

### Kill any stuck helper/app processes
```powershell
taskkill /F /IM noland-net-helper.exe
taskkill /F /IM noland-connect.exe
```

### Remove stale IP from the adapter if it exists
```powershell
Get-NetIPAddress -InterfaceAlias "nolandwg0" -AddressFamily IPv4 -ErrorAction SilentlyContinue | Remove-NetIPAddress -Confirm:$false
```

### Remove stale routes on that adapter
```powershell
Get-NetRoute -InterfaceAlias "nolandwg0" -ErrorAction SilentlyContinue | Remove-NetRoute -Confirm:$false
```

### Bounce the adapter if it exists
```powershell
Disable-NetAdapter -Name "nolandwg0" -Confirm:$false -ErrorAction SilentlyContinue
Start-Sleep -Seconds 2
Enable-NetAdapter -Name "nolandwg0" -Confirm:$false -ErrorAction SilentlyContinue
```

Then launch Noland **normally** again, not as admin.

---

## If it still fails

Run this and send me the output:

```powershell
Get-NetAdapter -IncludeHidden | Where-Object { $_.Name -like "nolandwg*" } | Format-List *
Get-NetIPAddress -InterfaceAlias "nolandwg0" -AddressFamily IPv4 -ErrorAction SilentlyContinue | Format-List *
Get-NetRoute -InterfaceAlias "nolandwg0" -ErrorAction SilentlyContinue | Format-List *
Get-Process | Where-Object { $_.ProcessName -like "noland-net-helper*" -or $_.ProcessName -like "noland-connect*" } | Format-List Id,ProcessName,Path
Test-NetConnection 10.77.0.1 -Port 47984
```

---

## Should you run the app as Administrator?

### Short answer:
**No, not as the real fix.**

### Only as a diagnostic:
If you run the main app as admin **once** and it suddenly works, that tells us:
- the failure is in the **elevation handoff / helper launch path**
- not in Sunshine, not in the remote Linux box, and not in Moonlight

That would be useful as a signal, but it is **not** the correct final behavior.

---

## Where logs likely are

Check these folders:

- `%APPDATA%\com.noland.connect\wireguard\`
- `%LOCALAPPDATA%\Noland Connect\`
- `%APPDATA%\com.noland.connect\logs\` if present

Especially anything under:
- `wireguard\<instance-id>\`
- `gotatun-runtime`

---

## My current read

Given everything so far, the likely problem is:

1. helper gets elevated
2. `nolandwg0` already exists in a partially configured state
3. Windows rejects reassigning the same address
4. app retries / verification fails
5. you see the loop/failure behavior

So the next step is to **inspect and clean stale Windows adapter state**, not to ask users to manually install anything.

If you want, I can now help you do this in two tracks:

1. **debug your current Windows machine step-by-step**, or  
2. **patch `no_land` so retries do hard cleanup of stale `nolandwg0` state before setup**.
