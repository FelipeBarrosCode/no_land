with open("src-tauri/src/services/mic_passthrough.rs", "r") as f:
    code = f.read()

code = code.replace("sample_rate().0", "sample_rate().0") # ahhh!
code = code.replace("sample_rate().0", "sample_rate()") # this is correct!

with open("src-tauri/src/services/mic_passthrough.rs", "w") as f:
    f.write(code)
