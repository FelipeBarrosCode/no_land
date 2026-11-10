"""Focused tests for the fixed SSH-side helper. No VM, credentials or shell server."""
import base64
import hashlib
import importlib.util
import os
from pathlib import Path
import tempfile
import unittest

SOURCE = Path(__file__).resolve().parents[2] / "src-tauri/src/mcp/remote.py"
SPEC = importlib.util.spec_from_file_location("mcp_remote", SOURCE)
REMOTE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(REMOTE)


class RemoteFiles(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="noland-mcp-test-")
        self.root = Path(self.directory.name)

    def tearDown(self):
        self.directory.cleanup()

    def run_file(self, mode, path, **kwargs):
        return REMOTE.run({"mode": mode, "path": str(path), **kwargs})

    def test_read_is_bounded_and_checksummed(self):
        path = self.root / "data"
        path.write_bytes(b"0123456789")
        result = self.run_file("files_read", path, offset=2, limit=3)
        self.assertEqual(base64.b64decode(result["base64"]), b"234")
        self.assertEqual(result["sha256"], hashlib.sha256(b"0123456789").hexdigest())

    def test_write_requires_matching_version_and_does_not_replace_symlinks(self):
        path = self.root / "data"
        self.run_file("files_write", path, content="first")
        with self.assertRaises(ValueError):
            self.run_file("files_write", path, content="unexpected overwrite")
        self.run_file("files_write", path, content="second", expected_sha256=hashlib.sha256(b"first").hexdigest())
        self.assertEqual(path.read_text(), "second")
        link = self.root / "link"
        link.symlink_to(path)
        with self.assertRaises(ValueError):
            self.run_file("files_write", link, content="no")
        self.assertEqual(path.read_text(), "second")

    def test_move_never_overwrites_an_existing_destination(self):
        source = self.root / "source"
        destination = self.root / "destination"
        source.write_text("source")
        destination.write_text("destination")
        with self.assertRaises(ValueError):
            self.run_file("files_move", source, destination=str(destination))
        self.assertEqual(destination.read_text(), "destination")

    def test_refuses_credentials_and_kernel_paths_including_symlinks(self):
        for value in ["relative.txt", "/proc/self/environ", "/etc/wireguard/wg0.conf", "/home/user/.ssh/id_ed25519"]:
            with self.assertRaises(ValueError):
                REMOTE.path_arg(value)
        link = self.root / "link"
        link.symlink_to("/etc/ssh")
        with self.assertRaises(ValueError):
            REMOTE.path_arg(str(link / "ssh_config"))

    def test_delete_refuses_system_and_home_roots(self):
        for value in ["/", str(Path.home())]:
            with self.assertRaises(ValueError):
                self.run_file("files_delete", value, recursive=True)

    def test_shell_metacharacters_in_paths_are_data(self):
        path = self.root / "file'; $(not-a-command)"
        self.run_file("files_write", path, content="literal")
        self.assertEqual(path.read_text(), "literal")
        self.assertEqual(len(list(self.root.iterdir())), 1)

    def test_job_ids_cannot_escape_the_job_directory(self):
        for value in ["../../file", "not-a-uuid", "0" * 200]:
            with self.assertRaises(ValueError):
                REMOTE.job_paths(value)


if __name__ == "__main__":
    unittest.main()
