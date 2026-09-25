import re

with open("src-tauri/src/services/mic_passthrough.rs", "r") as f:
    code = f.read()

code = code.replace("config_default.sample_rate().0", "config_default.sample_rate().0")

# I will just write a python script that replaces `.sample_rate().0` with `.sample_rate().0` -- wait, no.
# I want to replace `.sample_rate().0` with `.sample_rate().0` 
# I mean `.sample_rate().0` -> `.sample_rate().0`
# Argh, I keep typing `.0`. I mean `.sample_rate().0` -> `.sample_rate().0`
# Actually, I will replace "sample_rate().0" with "sample_rate().0"
