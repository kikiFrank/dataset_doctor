import { execFileSync } from "node:child_process";
import { appendFileSync } from "node:fs";
import { pathToFileURL } from "node:url";

export const allChecks = ["rust", "typescript", "python", "contracts", "sqlite", "integration", "desktop"];

export function selectChecks(paths, { all = false } = {}) {
  if (all) return [...allChecks];
  const selected = new Set();
  const add = (...names) => names.forEach(name => selected.add(name));
  for (const name of paths) {
    const path = name.replaceAll("\\", "/");
    if (/^(docs\/|README\.md$|AGENTS\.md$|LICENSE|tests\/rig-poc\/|\.github\/workflows\/rig-validation\.yml$)/.test(path)) continue;
    if (/^(\.github\/(actions|workflows)\/|scripts\/ci\/|tests\/ci\/|config\/toolchains\/|contracts\/|Cargo\.(toml|lock)$)/.test(path)) add(...allChecks);
    else if (/^apps\/desktop\/(src\/|index\.html|package\.json|pnpm-lock\.yaml|tsconfig|vite\.config)/.test(path) || path.startsWith("tests/desktop/ui/")) add("typescript", "desktop");
    else if (path.startsWith("services/python-worker/") || path.startsWith("tests/worker/")) add("python", "integration", "desktop");
    else if (path.startsWith("tests/storage/") || path.startsWith("scripts/storage/")) add("sqlite");
    else if (path.startsWith("crates/dd-infra/")) add("rust", "sqlite", "integration", "desktop");
    else if (path.startsWith("crates/dd-ui-contracts/")) add("rust", "typescript", "contracts", "desktop");
    else if (/^crates\/dd-(core|domain)\//.test(path)) add("rust", "sqlite", "integration", "desktop");
    else if (path.startsWith("crates/dd-protocol/") || path.startsWith("tests/contracts/")) add("rust", "typescript", "python", "contracts", "integration", "desktop");
    else if (path.startsWith("tests/integration/")) add("integration");
    else if (path.startsWith("scripts/contracts/")) add("contracts", "integration");
    else if (path.startsWith("tests/desktop/")) add("desktop");
    else if (/^(apps\/desktop\/src-tauri\/|scripts\/packaging\/|tests\/packaging\/)/.test(path)) add("rust", "desktop");
    else if (path.startsWith("apps/agent-service/")) add("rust", "integration", "desktop");
    else add(...allChecks);
  }
  return allChecks.filter(check => selected.has(check));
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const base = process.env.CI_BASE_SHA ?? "";
  const head = process.env.CI_HEAD_SHA ?? "";
  const all = process.env.CI_FULL_CHECK === "true" || /^0{40}$/.test(base);
  let paths = [];
  if (!all) {
    if (![base, head].every(value => /^[a-f0-9]{40}$/i.test(value))) throw new Error("Pass full Git base/head SHAs or request a full check");
    paths = execFileSync("git", ["diff", "--name-only", "--no-renames", "-z", base, head], { encoding: "utf8" }).split("\0").filter(Boolean);
  }
  const checks = selectChecks(paths, { all });
  const encoded = JSON.stringify(checks);
  if (process.env.GITHUB_OUTPUT) appendFileSync(process.env.GITHUB_OUTPUT, `checks=${encoded}\n`);
  process.stdout.write(encoded + "\n");
}
