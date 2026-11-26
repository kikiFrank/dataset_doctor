"""Create a disposable SQLite fixture for native Workspace IPC acceptance."""

from contextlib import closing
import json
from pathlib import Path
import sqlite3
import sys

root = Path(sys.argv[1])
repo = Path(__file__).resolve().parents[3]
workspace_id = "2c823c8a-1a3a-4b0d-97b3-0982e5f2aca9"
root.mkdir()
marker = {
    "config_format_version": 1,
    "config_revision": 7,
    "workspace_id": workspace_id,
    "name": "桌面验收 Workspace",
}
(root / "workspace.json").write_text(json.dumps(marker, ensure_ascii=False), encoding="utf-8")
ddl = (repo / "migrations/workspace/0001_initial.sql").read_text(encoding="utf-8")
with closing(sqlite3.connect(root / "workspace.db")) as connection, connection:
    connection.executescript(ddl)
    connection.execute("INSERT INTO schema_meta VALUES(1, ?, 1, 0, 0)", (workspace_id,))
    connection.execute(
        """INSERT INTO assets(asset_id, kind, location_kind, path, original_filename,
                              file_origin, availability, created_at_ms, updated_at_ms)
           VALUES ('76afdb2d-0900-4528-b303-0fca0f864235', 'IMAGE', 'WORKSPACE', 'missing.png', 'missing.png',
                   'IMPORTED', 'OFFLINE', 0, 0)"""
    )
