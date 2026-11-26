"""Check the real Workflow and immutable inputs produced by the WebView2 acceptance run."""

from hashlib import sha256
import json
from pathlib import Path
import sqlite3
import sys


def verify(root: Path) -> None:
    expected = json.loads((root / "input-proof.json").read_text(encoding="utf-8"))
    actual = {name: sha256((root / "Inputs" / name).read_bytes()).hexdigest() for name in expected}
    assert actual == expected, "Healthcheck changed source media or annotation"
    workspace = root / "Workspace"
    database = sqlite3.connect((workspace / "workspace.db").as_uri() + "?mode=ro", uri=True)
    with database:
        assert database.execute("PRAGMA integrity_check").fetchall() == [("ok",)]
        assert database.execute("PRAGMA foreign_key_check").fetchall() == []
        ((run, dataset, status, plan, report_path),) = database.execute(
            "SELECT workflow_run_id,dataset_id,status,plan_json,result_manifest_path FROM workflow_runs"
        ).fetchall()
        assert status == "WAITING_REVIEW"
        assert json.loads(plan)["plan_source"] == "LOCAL_DEFAULT"
        tasks = database.execute(
            "SELECT operation_kind,status FROM tasks WHERE workflow_run_id=? ORDER BY operation_kind",
            (run,),
        ).fetchall()
        assert tasks == [
            ("ANNOTATION_GEOMETRY", "SUCCEEDED"), ("BUILD_REPORT", "SUCCEEDED"),
            ("DATASET_PROFILE", "SUCCEEDED"), ("WORKFLOW_PLAN_PREVIEW", "SUCCEEDED"),
        ]
        ((task, metadata, task_output),) = database.execute(
            "SELECT task_id,execution_metadata_json,output_manifest_path FROM tasks "
            "WHERE workflow_run_id=? AND operation_kind='BUILD_REPORT'", (run,),
        ).fetchall()
        assert report_path == task_output == f"artifacts/tasks/{task}/report.json"
        assert not database.execute("SELECT 1 FROM review_sessions").fetchall()
    database.close()
    report_bytes = (workspace / report_path).read_bytes()
    reference = json.loads(metadata)["terminal_receipt"]["result_manifest_ref"]
    assert reference["owner"] == {"kind": "TASK", "id": task}
    assert reference["sha256"] == sha256(report_bytes).hexdigest()
    assert reference["size_bytes"] == str(len(report_bytes))
    report = json.loads(report_bytes)
    assert report["report_id"] == reference["artifact_id"]
    assert report["scope"] == {"workflow_run_id": run, "dataset_id": dataset}
    assert report["human_ack_status"] == "PENDING"
    checks = {entry["check_id"]: entry for entry in report["checks"]}
    geometry = checks["annotation_geometry"]
    assert geometry["processed_assets"] == geometry["total_assets"] == 1
    assert any(finding["finding_code"] == "bbox_out_of_bounds" for finding in geometry["findings"])
    visual = checks["semantic_review"]
    assert visual["assessment"] == "NOT_EVALUATED"
    assert visual["skip_or_failure_reason"]["code"] == "MODEL_NOT_CONFIGURED"
    assert not any((root / "Exports").iterdir())
    (root / "workflow-evidence.json").write_text(json.dumps({
        "platform": "Windows", "workflow_run_id": run, "dataset_id": dataset, "status": status,
        "task_statuses": tasks, "report_id": report["report_id"], "report_sha256": reference["sha256"],
        "verified_source_sha256": actual,
    }, indent=2) + "\n", encoding="utf-8")
    print("Native local workflow, report and human approval gate verified")


if __name__ == "__main__":
    verify(Path(sys.argv[1]).resolve(strict=True))
