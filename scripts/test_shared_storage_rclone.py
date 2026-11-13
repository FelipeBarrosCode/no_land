import contextlib
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch
import zipfile


sys.dont_write_bytecode = True
ROOT = Path(os.environ.get("NOLAND_RCLONE_TEST_ROOT", Path(__file__).resolve().parents[1])).resolve()
SPEC = importlib.util.spec_from_file_location(
    "shared_storage_rclone", ROOT / "src-tauri/scripts/ensure_shared_storage_rclone.py"
)
rclone = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(rclone)
FLAGS = "      --stat  Stat one object\n      --no-mimetype  Skip MIME\n      --no-modtime  Skip time\n"


class RcloneCompatibility(unittest.TestCase):
    def probe(self, version, flags=FLAGS):
        with patch.object(rclone.shutil, "which", return_value="/usr/bin/rclone"), \
             patch.object(rclone.subprocess, "run", side_effect=[
                 subprocess.CompletedProcess([], 0, "rclone v" + version + "\n", ""),
                 subprocess.CompletedProcess([], 0, flags, ""),
             ]) as run:
            result = rclone.compatible()
            self.assertTrue(all(call.kwargs["check"] for call in run.call_args_list))
            self.assertTrue(all(call.kwargs["timeout"] == 12 for call in run.call_args_list))
            return result

    def test_distro_1533_is_not_accepted_just_because_it_exists(self):
        self.assertFalse(self.probe("1.53.3"))

    def test_tested_version_and_newer_stable_versions_are_accepted(self):
        for version in ["1.76.0", "1.76.1", "1.77.0", "2.0.0"]:
            with self.subTest(version=version):
                self.assertTrue(self.probe(version))

    def test_prerelease_malformed_and_older_versions_are_rejected(self):
        for version in ["1.75.9", "1.76.0-DEV", "1.77.0-beta.1", "garbage"]:
            with self.subTest(version=version):
                self.assertFalse(self.probe(version))

    def test_each_required_listing_flag_is_verified(self):
        for flag in ["--stat", "--no-mimetype", "--no-modtime"]:
            with self.subTest(flag=flag):
                self.assertFalse(self.probe("1.76.0", FLAGS.replace(flag, "--unrelated")))

    def test_a_flag_in_an_error_message_is_not_a_capability(self):
        self.assertFalse(self.probe("1.76.0", "Error: unknown flag: --stat\n" + FLAGS.replace("--stat", "--unrelated")))

    def test_missing_binary_and_failed_probes_are_not_success(self):
        with patch.object(rclone.shutil, "which", return_value=None):
            self.assertFalse(rclone.compatible())
        for error in [FileNotFoundError(), subprocess.CalledProcessError(1, "rclone"),
                      subprocess.TimeoutExpired("rclone", 12)]:
            with self.subTest(error=type(error)), patch.object(rclone.subprocess, "run", side_effect=error):
                self.assertFalse(rclone.compatible("/fixture/rclone"))


class RcloneDownload(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="rclone download test ")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)

    def archive(self, architecture="amd64", member=None, symlink=False):
        output = io.BytesIO()
        with zipfile.ZipFile(output, "w") as archive:
            entry = zipfile.ZipInfo(member or "rclone-v{}-linux-{}/rclone".format(rclone.VERSION, architecture))
            entry.external_attr = ((stat.S_IFLNK if symlink else stat.S_IFREG) | 0o755) << 16
            archive.writestr(entry, b"opaque executable fixture; never run")
            archive.writestr("unrelated-documentation", b"not extracted")
        return output.getvalue()

    def download(self, data, architecture="x86_64", expected=None, compatible=True):
        def curl(args, **kwargs):
            self.assertEqual(args[0], "curl")
            self.assertEqual(args[args.index("--proto") + 1], "=https")
            self.assertEqual(args[args.index("--proto-redir") + 1], "=https")
            self.assertTrue(args[-1].startswith("https://downloads.rclone.org/v1.76.0/"))
            Path(args[args.index("--output") + 1]).write_bytes(data)
            return subprocess.CompletedProcess(args, 0)
        with patch.object(rclone.platform, "machine", return_value=architecture), \
             patch.object(rclone, "DOWNLOADS", {
                 architecture: ("arm64" if architecture == "aarch64" else "amd64",
                                expected or hashlib.sha256(data).hexdigest())
             }), patch.object(rclone.subprocess, "run", side_effect=curl), \
             patch.object(rclone, "compatible", return_value=compatible):
            return rclone.download_candidate(self.root)

    def test_verified_x64_and_arm64_archives_extract_only_the_binary(self):
        for arch, archive_arch in [("x86_64", "amd64"), ("aarch64", "arm64")]:
            with self.subTest(architecture=arch):
                binary = self.download(self.archive(archive_arch), architecture=arch)
                self.assertEqual(binary.read_bytes(), b"opaque executable fixture; never run")
                self.assertEqual(stat.S_IMODE(binary.stat().st_mode), 0o700)
                self.assertFalse((self.root / "unrelated-documentation").exists())
                binary.unlink()

    def test_checksum_mismatch_never_extracts_or_executes_a_candidate(self):
        with self.assertRaisesRegex(RuntimeError, "checksum mismatch"):
            self.download(self.archive(), expected="0" * 64)
        self.assertFalse((self.root / "rclone").exists())

    def test_missing_archive_executable_is_rejected(self):
        with self.assertRaises(KeyError):
            self.download(self.archive(member="unexpected/rclone"))

    def test_symlink_in_verified_archive_is_rejected(self):
        with self.assertRaisesRegex(RuntimeError, "Invalid rclone executable"):
            self.download(self.archive(symlink=True))

    def test_downloaded_binary_must_also_pass_capability_probe(self):
        with self.assertRaisesRegex(RuntimeError, "required Shared Storage commands"):
            self.download(self.archive(), compatible=False)

    def test_unsupported_architecture_never_downloads(self):
        with patch.object(rclone.platform, "machine", return_value="riscv64"), \
             patch.object(rclone.subprocess, "run") as run:
            with self.assertRaisesRegex(RuntimeError, "only Linux x86-64 and ARM64"):
                rclone.download_candidate(self.root)
            run.assert_not_called()


class RcloneAdmission(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="rclone admission test ")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)

    def health(self, value):
        connection = Mock()
        connection.__enter__ = Mock(return_value=connection)
        connection.__exit__ = Mock(return_value=False)
        response = Mock()
        response.__enter__ = Mock(return_value=response)
        response.__exit__ = Mock(return_value=False)
        response.readline.return_value = json.dumps(value).encode() + b"\n"
        connection.makefile.return_value = response
        agent_socket = self.root / "socket-present"
        agent_socket.touch()
        with patch.object(rclone, "AGENT_SOCKET", agent_socket), \
             patch.object(rclone, "PROC", self.root), \
             patch.object(rclone.socket, "socket", return_value=connection):
            rclone.require_idle()
        request = json.loads(connection.sendall.call_args.args[0])
        self.assertEqual(request["method"], "GetHealth")

    def test_idle_agent_permits_installation_without_mutating_rpc(self):
        self.health({"result": {"active_operations": 0}})

    def test_busy_or_ambiguous_agent_never_permits_upgrade(self):
        for active in [1, None, "0", False]:
            with self.subTest(active=active), self.assertRaisesRegex(RuntimeError, "Shared Storage is busy"):
                self.health({"result": {"active_operations": active}})

    def test_unresponsive_agent_never_permits_upgrade(self):
        agent_socket = self.root / "socket-present"
        agent_socket.touch()
        with patch.object(rclone, "AGENT_SOCKET", agent_socket), \
             patch.object(rclone.socket, "socket", side_effect=OSError("unresponsive")):
            with self.assertRaisesRegex(RuntimeError, "Cannot verify Shared Storage is idle"):
                rclone.require_idle()

    def test_running_rclone_blocks_upgrade_even_without_agent_socket(self):
        process = self.root / "1234"
        process.mkdir()
        (process / "comm").write_text("rclone\n")
        with patch.object(rclone, "AGENT_SOCKET", self.root / "no-socket"), \
             patch.object(rclone, "PROC", self.root):
            with self.assertRaisesRegex(RuntimeError, "transfer is running"):
                rclone.require_idle()

    def test_publish_is_atomic_and_preserves_unrelated_files(self):
        candidate = self.root / "candidate"
        candidate.write_bytes(b"verified fixture bytes")
        target = self.root / "bin/rclone"
        target.parent.mkdir()
        unrelated = target.parent / "unrelated"
        unrelated.write_text("keep")
        with patch.object(rclone, "TARGET", target), patch.object(rclone, "trusted_directory"), \
             patch.object(rclone.os, "replace", wraps=os.replace) as replace:
            rclone.publish(candidate)
        self.assertEqual(target.read_bytes(), candidate.read_bytes())
        self.assertEqual(stat.S_IMODE(target.stat().st_mode), 0o755)
        self.assertEqual(unrelated.read_text(), "keep")
        self.assertEqual(replace.call_count, 1)
        self.assertEqual(Path(replace.call_args.args[0]).parent, target.parent)
        self.assertEqual(sorted(p.name for p in target.parent.iterdir()), ["rclone", "unrelated"])

    def test_publish_does_not_follow_destination_symlink(self):
        victim = self.root / "keep"
        victim.write_text("untouched")
        target = self.root / "rclone"
        target.symlink_to(victim)
        with patch.object(rclone, "TARGET", target), patch.object(rclone, "trusted_directory"):
            with self.assertRaisesRegex(RuntimeError, "unmanaged"):
                rclone.publish(victim)
        self.assertEqual(victim.read_text(), "untouched")

    def test_writable_install_directory_is_rejected(self):
        directory = self.root / "unsafe"
        directory.mkdir(mode=0o777)
        directory.chmod(0o777)
        with self.assertRaisesRegex(RuntimeError, "root-owned"):
            rclone.trusted_directory(directory)

    def test_lock_file_symlink_is_rejected_without_touching_target(self):
        victim = self.root / "keep"
        victim.write_text("untouched")
        lock = self.root / "lock"
        lock.symlink_to(victim)
        with patch.object(rclone, "LOCK", lock), self.assertRaises(OSError):
            with rclone.installation_lock():
                self.fail("symlink lock must not be acquired")
        self.assertEqual(victim.read_text(), "untouched")


class RcloneEnsure(unittest.TestCase):
    def run_ensure(self, probes, idle_errors=None, download_error=None):
        with patch.object(rclone.os, "geteuid", return_value=0), \
             patch.object(rclone, "compatible", side_effect=probes), \
             patch.object(rclone, "installation_lock", return_value=contextlib.nullcontext()) as lock, \
             patch.object(rclone, "require_idle", side_effect=idle_errors) as idle, \
             patch.object(rclone, "download_candidate", side_effect=download_error, return_value=Path("/fixture/rclone")) as download, \
             patch.object(rclone, "publish") as publish, \
             contextlib.redirect_stdout(io.StringIO()) as output:
            error = None
            try:
                rclone.ensure()
            except RuntimeError as caught:
                error = caught
        return error, output.getvalue(), lock, idle, download, publish

    def test_compatible_install_is_read_only_even_during_other_operations(self):
        error, output, lock, idle, download, publish = self.run_ensure([True])
        self.assertIsNone(error)
        self.assertEqual(output, "RCLONE_READY\n")
        for operation in [lock, idle, download, publish]:
            operation.assert_not_called()

    def test_concurrent_request_rechecks_after_installation_lock(self):
        error, output, lock, idle, download, publish = self.run_ensure([False, True])
        self.assertIsNone(error)
        self.assertEqual(output, "RCLONE_READY\n")
        lock.assert_called_once()
        for operation in [idle, download, publish]:
            operation.assert_not_called()

    def test_upgrade_rechecks_idle_and_final_privileged_path(self):
        error, output, lock, idle, download, publish = self.run_ensure([False, False, True])
        self.assertIsNone(error)
        self.assertEqual(output, "RCLONE_READY\n")
        self.assertEqual(idle.call_count, 2)
        publish.assert_called_once_with(Path("/fixture/rclone"))

    def test_busy_before_or_after_download_never_publishes(self):
        for errors in [[RuntimeError("busy")], [None, RuntimeError("busy")]]:
            with self.subTest(stage=len(errors)):
                error, output, lock, idle, download, publish = self.run_ensure([False, False], idle_errors=errors)
                self.assertIsNotNone(error)
                self.assertEqual(output, "")
                publish.assert_not_called()

    def test_failed_archive_verification_never_publishes(self):
        error, output, lock, idle, download, publish = self.run_ensure(
            [False, False], download_error=RuntimeError("checksum mismatch")
        )
        self.assertRegex(str(error), "checksum mismatch")
        self.assertEqual(output, "")
        publish.assert_not_called()

    def test_wrong_privileged_path_is_not_reported_as_success(self):
        error, output, lock, idle, download, publish = self.run_ensure([False, False, False])
        self.assertRegex(str(error), "sudo PATH")
        self.assertEqual(output, "")

    def test_nonroot_cannot_install(self):
        with patch.object(rclone.os, "geteuid", return_value=1000), patch.object(rclone, "publish") as publish:
            with self.assertRaisesRegex(RuntimeError, "sudo"):
                rclone.ensure()
            publish.assert_not_called()


if __name__ == "__main__":
    unittest.main(verbosity=2)
