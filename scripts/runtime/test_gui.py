"""Offline tests; never launch dropped files, install packages, or alter the host."""
import importlib.util
import json
from pathlib import Path
import subprocess
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("runtime_gui", ROOT / "runtime-agent/src/gui.py")
gui = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gui)


class DropTests(unittest.TestCase):
    def test_literal_local_filename_with_spaces_and_shell_characters(self):
        self.assertEqual(gui.dropped_paths(["file:///home/user/O%27Brien%3B%20%24%28touch%20nope%29.AppImage"]),
            ["/home/user/O'Brien; $(touch nope).AppImage"])

    def test_localhost_uris(self):
        self.assertEqual(gui.dropped_paths(["file://localhost/tmp/app"]), ["/tmp/app"])

    def test_remote_urls_and_control_characters_are_not_applications(self):
        for uri in ["https://example.com/app", "file://other-pc/tmp/app", "file:relative",
                    "file:///tmp/app%00", "file:///tmp/app%0A", "file:///tmp/app%0D"]:
            with self.subTest(uri=uri), self.assertRaises(ValueError):
                gui.dropped_paths([uri])

    def test_one_file_at_a_time(self):
        for uris in [[], ["file:///tmp/one", "file:///tmp/two"]]:
            with self.assertRaises(ValueError):
                gui.dropped_paths(uris)

    def test_engine_receives_an_argument_array_not_a_shell(self):
        path = "/home/user/O'Brien; $(touch nope).AppImage"
        with patch.object(gui.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, '{"id":"app"}', "")) as run:
            self.assertEqual(gui.engine_call("/engine", "register", "--file", path), {"id": "app"})
            self.assertEqual(run.call_args.args[0], ["/engine", "register", "--file", path])
            self.assertNotIn("shell", run.call_args.kwargs)

    def test_engine_error_is_not_reported_as_success(self):
        error = json.dumps({"error": "Runtime diagnostics did not pass"})
        with patch.object(gui.subprocess, "run", return_value=subprocess.CompletedProcess([], 1, "", error)):
            with self.assertRaisesRegex(RuntimeError, "diagnostics did not pass"):
                gui.engine_call("/engine", "launch", "--app-id", "app")


if __name__ == "__main__":
    unittest.main()
