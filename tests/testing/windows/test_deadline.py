"""Real Windows processes; no mocked clocks, process handles, or executors."""
import importlib.util
from pathlib import Path
import sys
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[3]
SPEC = importlib.util.spec_from_file_location("bounded_test", ROOT / "scripts/testing/bounded_test.py")
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class DeadlineTests(unittest.TestCase):
    def test_windows_powershell_keeps_its_real_system_cmdlets(self):
        result = MODULE.run_bounded(
            ["powershell.exe", "-NoProfile", "-NonInteractive", "-Command",
             "$ErrorActionPreference='Stop'; Get-FileHash -LiteralPath $env:SystemRoot\\System32\\kernel32.dll -Algorithm SHA256 | Out-Null"], 5,
        )
        self.assertEqual(result["exit_code"], 0, result["stderr"])

    def test_cargo_runner_does_not_discover_tests_in_an_ordinary_tool(self):
        process = subprocess.run(
            [sys.executable, "-I", "-B", str(ROOT / "scripts/testing/bounded_test.py"),
             "--kind", "rust", "--", sys.executable, "-I", "-B", "-c",
             "import sys; assert len(sys.argv)==1; print('actual tool')"],
            capture_output=True, timeout=5,
        )
        self.assertEqual(process.returncode, 0, process.stderr.decode("utf-8", "replace"))
        self.assertIn(b"actual tool", process.stdout)

    def test_real_success_preserves_output_and_records_duration(self):
        result = MODULE.run_bounded([sys.executable, "-I", "-B", "-c", "print('actual child')"], 3)
        self.assertEqual(result["exit_code"], 0)
        self.assertFalse(result["timed_out"])
        self.assertIn(b"actual child", result["stdout"])
        self.assertLess(result["elapsed_seconds"], 3)

    def test_real_failure_is_not_reported_as_pass(self):
        result = MODULE.run_bounded([sys.executable, "-I", "-B", "-c", "raise SystemExit(7)"], 3)
        self.assertEqual(result["exit_code"], 7)
        self.assertFalse(result["timed_out"])

    def test_timeout_stops_real_descendants(self):
        output = ROOT / "target/ci/deadline-tests"
        output.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=output) as folder:
            marker = Path(folder) / "child.pid"
            program = (
                "import subprocess,sys,time,pathlib; "
                "p=subprocess.Popen([sys.executable,'-I','-B','-c','import time;time.sleep(60)']); "
                f"pathlib.Path({str(marker)!r}).write_text(str(p.pid)); time.sleep(60)"
            )
            result = MODULE.run_bounded([sys.executable, "-I", "-B", "-c", program], 2)
            self.assertTrue(marker.is_file(), "The real descendant must start before timeout")
            self.assertEqual(result["exit_code"], 124)
            self.assertTrue(result["timed_out"])
            self.assertEqual(result["remaining_processes"], 0)
            self.assertLess(result["elapsed_seconds"], 3)

    def test_normal_exit_also_reaps_an_orphan(self):
        program = "import subprocess,sys;subprocess.Popen([sys.executable,'-I','-B','-c','import time;time.sleep(60)'],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)"
        result = MODULE.run_bounded([sys.executable, "-I", "-B", "-c", program], 3)
        self.assertEqual(result["exit_code"], 0)
        self.assertEqual(result["remaining_processes"], 0)

    def test_invalid_or_over_30_second_deadlines_are_rejected(self):
        for seconds in (0, -1, 30.1, float("nan"), float("inf")):
            with self.subTest(seconds=seconds), self.assertRaises(ValueError):
                MODULE.run_bounded([sys.executable, "-c", "raise SystemExit(0)"], seconds)


if __name__ == "__main__":
    unittest.main()
