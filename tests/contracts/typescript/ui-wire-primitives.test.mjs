import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";

import { parseDecimalI64, parseDecimalU64, readArtifactOwner } from
  "../../../apps/desktop/src/api/wire-primitives.ts";

const casesUrl = new URL("../../../testdata/contracts/ui/v1/decimal/cases.json", import.meta.url);
const fixtures = JSON.parse(await readFile(casesUrl, "utf8"));

for (const { id, profile, raw, accepted } of fixtures) {
  test(`${profile}/${id}`, () => {
    assert.ok(profile === "i64" || profile === "u64");
    const parse = profile === "u64" ? parseDecimalU64 : parseDecimalI64;
    const value = JSON.parse(raw);
    if (accepted) {
      assert.equal(parse(value).toString(), value);
    } else {
      assert.throws(() => parse(value));
    }
  });
}

test("Artifact owner rejects an unknown discriminant or missing id", () => {
  for (const kind of ["TASK", "ASSET", "REVIEW"]) {
    const owner = { kind, id: "2c823c8a-1a3a-4b0d-97b3-0982e5f2aca9" };
    assert.deepEqual(readArtifactOwner(owner), owner);
  }
  assert.throws(() => readArtifactOwner({ kind: "UNKNOWN", id: "a" }));
  assert.throws(() => readArtifactOwner({ kind: "TASK" }));
});
