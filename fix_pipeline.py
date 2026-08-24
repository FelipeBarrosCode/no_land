with open("src-tauri/src/microphone/pipeline.rs", "r") as f:
    code = f.read()

code = code.replace(
    'audio/x-raw,format=F32LE,rate=48000,channels=1',
    'audio/x-raw,format=S16LE,rate=48000,channels=1'
)

with open("src-tauri/src/microphone/pipeline.rs", "w") as f:
    f.write(code)

