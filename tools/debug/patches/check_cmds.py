import re

with open("src-tauri/src/commands/mod.rs", "r") as f:
    code = f.read()

print("Found mic_client in commands/mod.rs:")
for line in code.splitlines():
    if "mic_client" in line:
        print(line)

