import re

with open("src-tauri/src/microphone/pipeline.rs", "r") as f:
    code = f.read()

code = code.replace('args.push("!".to_string());\n        args.push("rtpbin.recv_rtcp_sink_0".to_string());', 'args.push("!".to_string());\n        args.push("application/x-rtcp".to_string());\n        args.push("!".to_string());\n        args.push("rtpbin.recv_rtcp_sink_0".to_string());')

with open("src-tauri/src/microphone/pipeline.rs", "w") as f:
    f.write(code)

