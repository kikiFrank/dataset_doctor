import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";

const archive = process.argv[2];
if (!archive) throw new Error("Pass the actual tracked source tar archive");
const deadline = performance.now() + 29_000;
const remaining = () => ({ timeout: Math.max(1, Math.floor(deadline - performance.now())) });
// Run from the checkout containing .git, before using the extracted source.
// These consumers require exact LF bytes, irrespective of runner Git config.
for (const path of [
  "tests/storage/sqlite/file-lock/Cargo.lock",
  "services/python-worker/src/dd_worker/generated/datasetdoctor/v1/core_execution_pb2.py",
]) {
  const expected = execFileSync("git", ["show", `HEAD:${path}`], remaining());
  const actual = execFileSync("tar.exe", ["-xOf", archive, path], remaining());
  assert.deepEqual(actual, expected, `CI source archive changed tracked bytes: ${path}`);
}
console.log("CI_SOURCE_ARCHIVE_BYTES_PASSED 2");
