"""Generate measured metadata. Signing is a separate CI step, never an installer bypass."""
import hashlib
import json
import os
from pathlib import Path
import tarfile

archive = Path("dist/runtime/ubuntu24-x86_64.tar.gz")
with archive.open("rb") as stream:
    digest = hashlib.file_digest(stream, "sha256").hexdigest()
with tarfile.open(archive, "r:gz") as tar:
    extracted = 0
    for member in tar:
        assert member.isfile() or member.isdir(), f"Unexpected archive type: {member.name}"
        assert not member.name.startswith("/") and ".." not in Path(member.name).parts
        extracted += member.size
manifest = {
    "schema": 1,
    "id": os.environ["RUNTIME_ID"],
    "profile": "ubuntu24-x86_64",
    "url": os.environ["ARTIFACT_URL"],
    "sha256": digest,
    "downloadBytes": archive.stat().st_size,
    "extractedBytes": extracted,
}
archive.with_name("manifest.json").write_text(json.dumps(manifest, sort_keys=True) + "\n")
