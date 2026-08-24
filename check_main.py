import re

with open("src-tauri/src/main.rs", "r") as f:
    code = f.read()

print("Found mic_client in main.rs:")
for line in code.splitlines():
    if "mic_client" in line:
        print(line)

