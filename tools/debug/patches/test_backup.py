import socket, sys, base64, json, uuid
session = {
    "operation_id": str(uuid.uuid4()),
    "provider": "google_drive",
    "backend_type": "drive",
    "remote_name": "noland_test",
    "root": "Noland",
    "config_ini": "[noland_test]\ntype = local\n",
    "expires_at_unix": 1787057759
}
req = {
    "id": "1",
    "method": "StartBackup",
    "params": {
        "app_id": "*",
        "mode": "personal_state",
        "session": session,
        "master_key_hex": "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20"
    }
}
print(base64.b64encode(json.dumps(req).encode()).decode())
