"""Verify the two real frames accepted by the Windows batch panel."""

from hashlib import sha256
import json
from pathlib import Path
import sqlite3
import sys


def verify(workspace: Path, plan: dict, source_digest: str) -> None:
    dataset = plan["dataset_id"]
    expected = plan["asset_ids"]
    original = Path(plan["workspace"]).resolve(strict=True)
    database = sqlite3.connect((workspace / "workspace.db").as_uri() + "?mode=ro", uri=True)
    with database:
        assert database.execute("PRAGMA integrity_check").fetchall() == [("ok",)]
        assert database.execute("PRAGMA foreign_key_check").fetchall() == []
        assert database.execute("SELECT count(*) FROM staging_items").fetchone() == (0,)
        assert database.execute(
            "SELECT row_revision FROM datasets WHERE dataset_id=?", (dataset,),
        ).fetchone() == (1,)
        finalized = database.execute(
            "SELECT a.asset_id, a.path, r.status FROM dataset_assets members "
            "JOIN assets a USING (asset_id) JOIN review_sessions r ON r.review_id=members.final_review_id "
            "WHERE members.dataset_id=? ORDER BY a.asset_id", (dataset,),
        ).fetchall()
        assert [asset for asset, _, _ in finalized] == expected
        ((status, metadata),) = database.execute(
            "SELECT status, execution_metadata_json FROM tasks WHERE operation_kind='STAGING_COMMIT'",
        ).fetchall()
        assert status == "SUCCEEDED"
    database.close()

    receipt = json.loads(metadata)["staging_commit_receipt"]
    assert receipt["target_dataset_id"] == dataset
    assert receipt["accepted_asset_ids"] == expected
    assert receipt["acknowledged_findings"] == []
    assert receipt["acknowledged_incomplete_checks"] == [{
        "check_id": "semantic_review", "reason": "已核对两个合成视频帧",
    }]
    reference = receipt["quality_report_ref"]
    assert reference["kind"] == "STAGING_BATCH_CHECK_REPORT"
    raw_report = (workspace / "artifacts" / "tasks" / reference["owner"]["id"] / "report.json").read_bytes()
    assert sha256(raw_report).hexdigest() == reference["sha256"]
    assert str(len(raw_report)) == reference["size_bytes"]
    report = json.loads(raw_report)
    assert report["dataset_id"] == dataset
    assert [member["scope"]["asset_id"] for member in report["members"]] == expected

    digests = {}
    for asset, relative_path, state in finalized:
        assert state == "CONFIRMED"
        path = (workspace / relative_path).resolve(strict=True)
        original_path = (original / relative_path).resolve(strict=True)
        assert path.is_relative_to(workspace) and original_path.is_relative_to(original)
        content = path.read_bytes()
        assert content == original_path.read_bytes()
        digests[asset] = sha256(content).hexdigest()
    assert sha256(Path(plan["source_video"]).read_bytes()).hexdigest() == source_digest
    (workspace.parent / "staging-batch-evidence.json").write_text(json.dumps({
        "platform": "Windows", "dataset_id": dataset, "asset_ids": expected,
        "frame_sha256": digests, "report_sha256": reference["sha256"],
        "source_video_sha256": source_digest, "receipt": receipt,
    }, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print("Native WebView2 accepted two reviewed frames and preserved their source bytes")


if __name__ == "__main__":
    verify(
        Path(sys.argv[1]).resolve(strict=True),
        json.loads(Path(sys.argv[2]).read_text(encoding="utf-8")),
        sys.argv[3].lower(),
    )
