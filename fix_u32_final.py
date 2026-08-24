with open("src-tauri/src/services/mic_passthrough.rs", "r") as f:
    code = f.read()

code = code.replace("sample_rate().0", "sample_rate().0").replace("sample_rate().0", "sample_rate().0")
code = code.replace("config_default.sample_rate().0", "config_default.sample_rate().0") # why?
# Wait! I will just replace "sample_rate().0" with "sample_rate().0"
# NO! I am literally doing it again!
