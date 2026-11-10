import importlib.util
import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("runtime_export", ROOT / "export.py")
export = importlib.util.module_from_spec(spec)
spec.loader.exec_module(export)


class ArtifactTests(unittest.TestCase):
    def test_export_flattens_links_and_handles_directory_cycles(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "file").write_bytes(b"application")
            (root / "alias").symlink_to(root / "file")
            (root / "cycle").symlink_to(root, target_is_directory=True)
            result = io.BytesIO()
            with tarfile.open(fileobj=result, mode="w") as archive:
                export.emit(archive, root, "usr", set())
            result.seek(0)
            with tarfile.open(fileobj=result, mode="r") as archive:
                self.assertEqual(archive.getnames(), ["usr", "usr/alias", "usr/file"])
                self.assertTrue(all(m.isfile() or m.isdir() for m in archive))
                self.assertEqual(archive.extractfile("usr/alias").read(), b"application")

    def test_manifest_measures_real_download_and_extraction_sizes(self):
        with tempfile.TemporaryDirectory() as directory:
            dist = Path(directory) / "dist/runtime"
            dist.mkdir(parents=True)
            artifact = dist / "ubuntu24-x86_64.tar.gz"
            with tarfile.open(artifact, "w:gz") as archive:
                info = tarfile.TarInfo("usr/bin/app")
                info.size = 7
                archive.addfile(info, io.BytesIO(b"example"))
            env = dict(os.environ, RUNTIME_ID="test-v1", ARTIFACT_URL="https://example.com/runtime.tar.gz")
            subprocess.run([sys.executable, str(ROOT / "manifest.py")], cwd=directory, env=env, check=True)
            manifest = json.loads((dist / "manifest.json").read_text())
            self.assertEqual(manifest["downloadBytes"], artifact.stat().st_size)
            self.assertEqual(manifest["extractedBytes"], 7)
            self.assertEqual(manifest["profile"], "ubuntu24-x86_64")
            self.assertEqual(manifest["sha256"], hashlib.sha256(artifact.read_bytes()).hexdigest())

    def test_manifest_hashes_every_block_without_python311_file_digest(self):
        with tempfile.TemporaryDirectory() as directory:
            dist = Path(directory) / "dist/runtime"
            dist.mkdir(parents=True)
            artifact = dist / "ubuntu24-x86_64.tar.gz"
            payload = b"x" * (2 * 1024 * 1024) + b"last block"
            with tarfile.open(artifact, "w:gz", compresslevel=0) as archive:
                info = tarfile.TarInfo("usr/bin/app")
                info.size = len(payload)
                archive.addfile(info, io.BytesIO(payload))
            env = dict(os.environ, RUNTIME_ID="test-v1", ARTIFACT_URL="https://example.com/runtime.tar.gz")
            bootstrap = (
                "import hashlib, runpy, sys; "
                "hashlib.__dict__.pop('file_digest', None); "
                "runpy.run_path(sys.argv[1], run_name='__main__')"
            )
            subprocess.run([sys.executable, "-c", bootstrap, str(ROOT / "manifest.py")],
                           cwd=directory, env=env, check=True)
            manifest = json.loads((dist / "manifest.json").read_text())
            self.assertEqual(manifest["sha256"], hashlib.sha256(artifact.read_bytes()).hexdigest())
            self.assertEqual(manifest["extractedBytes"], len(payload))


if __name__ == "__main__":
    unittest.main()
