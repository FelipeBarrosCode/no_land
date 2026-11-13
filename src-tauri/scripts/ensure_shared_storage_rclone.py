#!/usr/bin/env python3
"""Install only the verified rclone needed by Shared Storage; never restart services."""

import contextlib
import fcntl
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import socket
import stat
import subprocess
import sys
import tempfile
import zipfile


VERSION = "1.76.0"
MIN_VERSION = (1, 76, 0)
# Published at https://downloads.rclone.org/v1.76.0/SHA256SUMS.
DOWNLOADS = {
    "x86_64": ("amd64", "b00f00b3266c3764413afe599d3ec316f6f43b3e0ea8a0bc1d186d2133e46df7"),
    "aarch64": ("arm64", "ee169016675e23a69d77240565133ff7dbc9b33d16fa8f7053d4af39f1eaeb78"),
}
TARGET = Path("/usr/local/bin/rclone")
LOCK = Path("/run/noland-rclone-install.lock")
AGENT_SOCKET = Path("/run/noland/state-agent.sock")
PROC = Path("/proc")


def compatible(binary=None):
    binary = str(binary) if binary else shutil.which("rclone")
    if not binary:
        return False
    try:
        version = subprocess.run(
            [binary, "version"], check=True, capture_output=True, text=True, timeout=12
        ).stdout
        match = re.search(r"^rclone v(\d+)\.(\d+)\.(\d+)[ \t]*$", version, re.M)
        if not match or tuple(map(int, match.groups())) < MIN_VERSION:
            return False
        help_text = subprocess.run(
            [binary, "lsjson", "--help"],
            check=True, capture_output=True, text=True, timeout=12,
        ).stdout
        return all(re.search(r"(?m)^\s*" + flag + r"\b", help_text)
                   for flag in ("--stat", "--no-mimetype", "--no-modtime"))
    except (OSError, subprocess.SubprocessError):
        return False


def require_idle():
    # A read-only RPC: do not cancel operations or restart the running agent.
    if AGENT_SOCKET.exists():
        try:
            with socket.socket(socket.AF_UNIX) as connection:
                connection.settimeout(10)
                connection.connect(str(AGENT_SOCKET))
                request = {"id": "rclone-prerequisite", "method": "GetHealth", "params": {}}
                connection.sendall((json.dumps(request) + "\n").encode())
                with connection.makefile("rb") as response:
                    health = json.loads(response.readline(65536)).get("result", {})
            active = health.get("active_operations")
            if type(active) is not int or active != 0:
                raise RuntimeError("Shared Storage is busy; retry after its current operation finishes")
        except (OSError, ValueError) as error:
            raise RuntimeError("Cannot verify Shared Storage is idle; rclone was not changed") from error
    for comm in PROC.glob("[0-9]*/comm"):
        try:
            if comm.read_text().strip() == "rclone":
                raise RuntimeError("An rclone transfer is running; retry after it finishes")
        except FileNotFoundError:
            pass  # Process exited between listing and reading its name.


def trusted_directory(directory):
    directory.mkdir(mode=0o755, exist_ok=True)
    metadata = directory.lstat()
    if not stat.S_ISDIR(metadata.st_mode) or metadata.st_uid != 0 or metadata.st_mode & 0o022:
        raise RuntimeError("rclone installation requires root-owned, non-writable system directories")


@contextlib.contextmanager
def installation_lock():
    descriptor = os.open(LOCK, os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    try:
        metadata = os.fstat(descriptor)
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != 0 or metadata.st_mode & 0o077:
            raise RuntimeError("Untrusted rclone installation lock")
        fcntl.flock(descriptor, fcntl.LOCK_EX)
        yield
    finally:
        os.close(descriptor)


def download_candidate(directory):
    architecture = {"amd64": "x86_64", "arm64": "aarch64"}.get(platform.machine(), platform.machine())
    if architecture not in DOWNLOADS:
        raise RuntimeError("Shared Storage rclone supports only Linux x86-64 and ARM64")
    archive_arch, expected = DOWNLOADS[architecture]
    name = "rclone-v{}-linux-{}".format(VERSION, archive_arch)
    archive = directory / "rclone.zip"
    subprocess.run(
        ["curl", "--fail", "--silent", "--show-error", "--location", "--proto", "=https",
         "--proto-redir", "=https", "--tlsv1.2", "--connect-timeout", "15", "--max-time", "120",
         "--max-filesize", "134217728", "--output", str(archive),
         "https://downloads.rclone.org/v{}/{}.zip".format(VERSION, name)],
        check=True, timeout=130,
    )
    if hashlib.sha256(archive.read_bytes()).hexdigest() != expected:
        raise RuntimeError("rclone download checksum mismatch; existing rclone was not changed")
    candidate = directory / "rclone"
    with zipfile.ZipFile(archive) as bundle:
        entry = bundle.getinfo(name + "/rclone")
        if entry.is_dir() or stat.S_ISLNK(entry.external_attr >> 16) or entry.file_size > 268435456:
            raise RuntimeError("Invalid rclone executable in the verified archive")
        with bundle.open(entry) as source, candidate.open("xb") as output:
            shutil.copyfileobj(source, output)
    candidate.chmod(0o700)
    if not compatible(candidate):
        raise RuntimeError("Downloaded rclone does not support the required Shared Storage commands")
    return candidate


def publish(candidate):
    for directory in (TARGET.parent.parent, TARGET.parent):
        trusted_directory(directory)
    if TARGET.is_symlink() or (TARGET.exists() and
                               (not TARGET.is_file() or TARGET.stat().st_uid != 0)):
        raise RuntimeError("Refusing to replace an unmanaged rclone installation")
    # Replace the directory entry, not a running executable's inode. An existing
    # process would retain its binary; no process or service is stopped here.
    descriptor, temporary = tempfile.mkstemp(prefix=".noland-rclone-", dir=TARGET.parent)
    try:
        with os.fdopen(descriptor, "wb") as output, candidate.open("rb") as source:
            shutil.copyfileobj(source, output)
            os.fchmod(output.fileno(), 0o755)
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, TARGET)
        directory = os.open(TARGET.parent, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def ensure():
    if os.geteuid() != 0:
        raise RuntimeError("The Shared Storage rclone prerequisite must run with sudo")
    if compatible():
        print("RCLONE_READY")
        return
    with installation_lock():
        # Concurrent catalog/test requests must not download or publish twice.
        if not compatible():
            require_idle()
            with tempfile.TemporaryDirectory(prefix="noland-rclone-") as temporary:
                candidate = download_candidate(Path(temporary))
                require_idle()
                publish(candidate)
            if not compatible():
                raise RuntimeError("Compatible rclone installed in /usr/local/bin, but sudo PATH still selects an incompatible copy")
    print("RCLONE_READY")


if __name__ == "__main__":
    try:
        ensure()
    except Exception as error:
        print("Shared Storage requires rclone >= {}: {}".format(VERSION, error), file=sys.stderr)
        sys.exit(1)
