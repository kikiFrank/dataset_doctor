"""Exit the real Worker after Core accepts its started event."""

import os
from pathlib import Path
import runpy
import sys


def crash_during_compute(_request):
    os._exit(17)


repo = Path(__file__).resolve().parents[4]
entrypoint = repo / "services" / "python-worker" / "src" / "dd_worker" / "entrypoint.py"
worker = runpy.run_path(str(entrypoint))
sys.modules["geometry_execution"].compute_result = crash_during_compute
raise SystemExit(worker["main"]())
