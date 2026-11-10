"""Export a flat-link, device-free image from INSIDE its build container.

Dereference files/directories with ancestor cycle detection (e.g. /usr/bin/X11
points to '.'). The installer consequently needs no absolute archive links.
"""
import os
import gzip
from pathlib import Path
import sys
import tarfile

EXCLUDED = {"etc/hosts", "etc/hostname", "etc/resolv.conf", "etc/mtab"}


def emit(archive, source, name, ancestors):
    if name in EXCLUDED:
        return
    source = Path(source)
    resolved = source.resolve()
    if not resolved.exists():
        return
    if resolved.is_dir():
        if resolved in ancestors:
            return
        info = tarfile.TarInfo(name)
        info.type = tarfile.DIRTYPE
        info.mode = 0o755
        archive.addfile(info)
        for child in sorted(source.iterdir(), key=lambda p: p.name):
            emit(archive, child, f"{name}/{child.name}", ancestors | {resolved})
    elif resolved.is_file():
        info = tarfile.TarInfo(name)
        info.mode = 0o755 if os.access(resolved, os.X_OK) else 0o644
        info.size = resolved.stat().st_size
        with resolved.open("rb") as stream:
            archive.addfile(info, stream)


if __name__ == "__main__":
    with gzip.GzipFile(fileobj=sys.stdout.buffer, mode="wb", mtime=0) as compressed:
        with tarfile.open(mode="w|", fileobj=compressed) as archive:
            for root in "bin sbin lib lib64 usr etc opt home var tmp run app".split():
                emit(archive, Path("/") / root, root, set())
