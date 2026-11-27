"""Windows test deadlines, including descendants; compilation stays outside."""
import argparse
import ctypes
from ctypes import wintypes
import json
import math
import os
from pathlib import Path
import struct
import subprocess
import sys
import time


class ProcessJob:
    def __init__(self):
        if os.name != "nt" or struct.calcsize("P") != 8:
            raise RuntimeError("This deadline entry is validated for Windows x64 only")
        self.api = ctypes.WinDLL("kernel32", use_last_error=True)
        signatures = {
            "CreateJobObjectW": ([ctypes.c_void_p, wintypes.LPCWSTR], wintypes.HANDLE),
            "SetInformationJobObject": ([wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD], wintypes.BOOL),
            "AssignProcessToJobObject": ([wintypes.HANDLE, wintypes.HANDLE], wintypes.BOOL),
            "TerminateJobObject": ([wintypes.HANDLE, wintypes.UINT], wintypes.BOOL),
            "QueryInformationJobObject": ([wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD, ctypes.c_void_p], wintypes.BOOL),
            "CloseHandle": ([wintypes.HANDLE], wintypes.BOOL),
        }
        for name, (arguments, result) in signatures.items():
            function = getattr(self.api, name)
            function.argtypes, function.restype = arguments, result
        self.handle = self.api.CreateJobObjectW(None, None)
        if not self.handle:
            raise ctypes.WinError(ctypes.get_last_error())
        limits = ctypes.create_string_buffer(144)  # Windows x64 extended limits
        struct.pack_into("I", limits, 16, 0x2000)  # KILL_ON_JOB_CLOSE
        try:
            self.check(self.api.SetInformationJobObject(self.handle, 9, limits, len(limits)))
        except BaseException:
            self.close()
            raise

    @staticmethod
    def check(success):
        if not success:
            raise ctypes.WinError(ctypes.get_last_error())

    def attach(self, process):
        self.check(self.api.AssignProcessToJobObject(self.handle, int(process._handle)))

    def stop(self):
        self.check(self.api.TerminateJobObject(self.handle, 124))

    def active(self):
        accounting = ctypes.create_string_buffer(48)
        self.check(self.api.QueryInformationJobObject(self.handle, 1, accounting, len(accounting), None))
        return struct.unpack_from("I", accounting, 40)[0]

    def close(self):
        if self.handle:
            self.api.CloseHandle(self.handle)
            self.handle = None


def run_bounded(command, seconds=30):
    if not math.isfinite(seconds) or not 0 < seconds <= 30:
        raise ValueError("A test deadline must be greater than zero and at most 30 seconds")
    if not command:
        raise ValueError("Pass an actual test executable and its arguments")
    started = time.monotonic()
    deadline = started + seconds
    job = ProcessJob()
    process = None
    timed_out = False
    try:
        process = subprocess.Popen(
            [sys.executable, "-I", "-B", str(Path(__file__).resolve()), "--broker"],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            creationflags=subprocess.CREATE_NO_WINDOW,
        )
        # The broker cannot start the actual command before receiving stdin.
        job.attach(process)
        try:
            stdout, stderr = process.communicate(
                json.dumps(command, ensure_ascii=True).encode("ascii") + b"\n",
                timeout=max(0.001, deadline - time.monotonic() - min(1, seconds / 10)),
            )
        except subprocess.TimeoutExpired:
            timed_out = True
            job.stop()
            stdout, stderr = process.communicate(timeout=max(0.001, deadline - time.monotonic()))
        # A successful parent must not leave a child behind either.
        job.stop()
        while job.active():
            if time.monotonic() >= deadline:
                raise TimeoutError("Test process tree did not stop within its deadline")
            time.sleep(0.005)
        return {
            "exit_code": 124 if timed_out else process.returncode,
            "timed_out": timed_out,
            "elapsed_seconds": time.monotonic() - started,
            "remaining_processes": 0,
            "stdout": stdout, "stderr": stderr,
        }
    finally:
        job.close()
        if process is not None:
            if process.poll() is None:
                process.kill()
            process.wait(timeout=1)


def emit(result, label):
    sys.stdout.buffer.write(result["stdout"])
    sys.stderr.buffer.write(result["stderr"])
    sys.stdout.flush()
    sys.stderr.flush()
    record = {key: value for key, value in result.items() if key not in ("stdout", "stderr")}
    record["test"] = label
    line = json.dumps(record, ensure_ascii=True)
    print("TEST_DEADLINE " + line, flush=True)
    directory = os.environ.get("DD_TEST_DEADLINE_EVIDENCE_DIR")
    if directory:
        path = Path(directory) / "test-durations.jsonl"
        path.parent.mkdir(parents=True, exist_ok=True)
        with path.open("a", encoding="utf-8") as output:
            output.write(line + "\n")


def main():
    if sys.argv[1:] == ["--broker"]:
        command = json.loads(sys.stdin.buffer.readline())
        environment = None
        if Path(command[0]).name.lower() == "powershell.exe":
            # A Python bridge does not perform pwsh's legacy-shell module-path
            # conversion. Let Windows PowerShell initialize its own defaults;
            # a bundled PowerShell 7 Utility module cannot run on .NET Framework.
            environment = {key: value for key, value in os.environ.items() if key.upper() != "PSMODULEPATH"}
        return subprocess.call(command, stdin=subprocess.DEVNULL, creationflags=subprocess.CREATE_NO_WINDOW, env=environment)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--kind", choices=("command", "rust"), default="command")
    parser.add_argument("--timeout-seconds", type=float, default=30)
    parser.add_argument("command", nargs=argparse.REMAINDER)
    options = parser.parse_args()
    command = options.command
    if command[:1] == ["--"]:
        command = command[1:]
    # Cargo also applies its runner to `cargo run` build/contract tools. Only
    # compiled libtest executables live in deps; never send discovery flags to
    # an ordinary executable such as export-ui-contracts.
    if options.kind == "rust" and command and Path(command[0]).parent.name == "deps":
        listing = run_bounded(command + ["--list", "--format=terse"], options.timeout_seconds)
        if listing["exit_code"]:
            emit(listing, "Rust test discovery")
            return listing["exit_code"]
        names = [line[:-6] for line in listing["stdout"].decode("utf-8").splitlines() if line.endswith(": test")]
        if names:
            for name in names:
                result = run_bounded(command + [name, "--exact"], options.timeout_seconds)
                emit(result, name)
                if result["exit_code"]:
                    return result["exit_code"]
                if b"1 passed; 0 failed; 0 ignored" not in result["stdout"]:
                    raise RuntimeError("The selected Rust test did not execute exactly once")
            return 0
    result = run_bounded(command, options.timeout_seconds)
    emit(result, Path(command[0]).name)
    return result["exit_code"]


if __name__ == "__main__":
    raise SystemExit(main())
