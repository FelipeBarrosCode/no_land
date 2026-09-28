import os, glob
log_dirs = [
    os.path.expanduser("~/Library/Logs/com.noland.connect"),
    os.path.expanduser("~/Library/Logs/noland-connect"),
]
for d in log_dirs:
    if os.path.exists(d):
        files = glob.glob(os.path.join(d, "*.log"))
        for f in files:
            print(f"--- {f} ---")
            os.system(f"tail -n 30 '{f}'")
