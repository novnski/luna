import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const evidence = path.dirname(fileURLToPath(import.meta.url));
const report = path.dirname(evidence);
const readJson = (name) => JSON.parse(fs.readFileSync(path.join(evidence, name), "utf8"));
const baselines = readJson("baselines.json");
const errors = [];
const warnings = [];
let checkedLinks = 0;
let checkedSourcePermalinks = 0;
const check = (ok, description) => { if (!ok) errors.push(description); };
const sha = (bytes) => crypto.createHash("sha256").update(bytes).digest("hex");
const git = (repo, args) => execFileSync("git", args, { cwd: baselines[repo].root, encoding: "utf8", maxBuffer: 16 * 1024 * 1024 });
const pinnedFile = (repo, revision, name) => git(repo, ["show", revision + ":" + name]);

for (const repo of ["t3", "bb"]) {
  for (const [key, expected] of [["harnessBaseline", "commitsSinceHarness"], ["uiBaseline", "commitsSinceUi"]]) {
    const count = Number(git(repo, ["rev-list", "--count", baselines[repo][key] + ".." + baselines[repo].head]).trim());
    check(count === baselines[repo][expected], repo + " commit range " + key);
  }
}
for (const name of fs.readdirSync(report).filter((n) => n.endsWith(".md"))) {
  const content = fs.readFileSync(path.join(report, name), "utf8");
  const defs = [...content.matchAll(/^\[([^\]]+)\]:\s+(\S+)/gm)];
  const defNames = new Set(defs.map((m) => m[1].toLowerCase()));
  for (const match of content.matchAll(/\[[^\]]+\]\[([^\]]+)\]/g)) {
    check(defNames.has(match[1].toLowerCase()), name + ": missing reference " + match[1]);
  }
  const links = new Set([...defs.map((m) => m[2]), ...[...content.matchAll(/\]\(([^)\s]+)\)/g)].map((m) => m[1])]);
  for (const target of links) {
    checkedLinks++;
    if (target.startsWith("https://")) {
      const m = target.match(/^https:\/\/github\.com\/(pingdotgg\/t3code|get-bb\/bb)\/blob\/([a-f0-9]{40})\/([^#]+)(?:#L(\d+))?$/);
      if (!m) { warnings.push(name + ": non-source URL not fetched: " + target); continue; }
      const repo = m[1] === "pingdotgg/t3code" ? "t3" : "bb";
      try {
        const source = pinnedFile(repo, m[2], decodeURIComponent(m[3]));
        check(!m[4] || Number(m[4]) <= source.split("\n").length, name + ": source line out of range " + target);
        checkedSourcePermalinks++;
      } catch {
        errors.push(name + ": unavailable source permalink " + target);
      }
    } else if (!target.startsWith("#")) {
      const raw = target.split("#")[0];
      const m = raw.match(/^(.*):(\d+)$/);
      const filename = m ? m[1] : raw;
      const full = path.isAbsolute(filename) ? filename : path.resolve(report, filename);
      const generatedTarget = process.argv.includes("--write") && [path.join(evidence, "validation.json"), path.join(report, "SHA256SUMS")].includes(full);
      check(fs.existsSync(full) || generatedTarget, name + ": missing local target " + target);
      if (m && fs.existsSync(full)) check(Number(m[2]) <= fs.readFileSync(full, "utf8").split("\n").length, name + ": local line out of range " + target);
    }
  }
}
const index = readJson("source-index.json");
const lunaDrift = [];
for (const item of index) {
  const repo = item.repo === "t3code" ? "t3" : item.repo;
  const current = fs.readFileSync(path.join(baselines[repo].root, item.path));
  if (repo === "luna") {
    if (sha(current) !== item.sha256) lunaDrift.push(item.path);
  } else {
    check(sha(Buffer.from(pinnedFile(repo, baselines[repo].head, item.path))) === item.sha256, "source fingerprint " + repo + "/" + item.path);
  }
}
if (lunaDrift.length) warnings.push("Luna source changed since audit: " + lunaDrift.join(", "));
const parent = "ecce5cc4-8416-4a84-aaf3-9e241fb13d91";
const fork = "18fa694d-dcd2-46f2-865f-ff02ac99b309";
const bindings = readJson("t3-provider-bindings.json");
const parentCodex = bindings.find((x) => x.thread_id === parent && x.driver === "codex");
const forkCodex = bindings.find((x) => x.thread_id === fork && x.driver === "codex");
check(Boolean(parentCodex && forkCodex), "Codex parent and fork bindings");
if (parentCodex && forkCodex) {
  check(JSON.parse(parentCodex.nativeThreadRef).nativeId !== JSON.parse(forkCodex.nativeThreadRef).nativeId, "distinct native fork identity");
  check(JSON.parse(forkCodex.forkedFrom).providerThreadId === parentCodex.provider_thread_id, "native fork source reference");
}
const handoffs = readJson("t3-handoffs.json");
for (const [strategy, delivery] of [["full_thread_summary", "inline"], ["delta_since_target_last_seen", "injected"], ["fork_delta_summary", "injected"]]) {
  check(handoffs.some((x) => x.strategy === strategy && x.deliveryStatus === delivery && x.transferredMessages > 0), "handoff " + strategy);
}
const transfers = readJson("t3-transfers.json");
for (const type of ["fork", "provider_handoff", "merge_back"]) check(transfers.some((x) => x.type === type && x.status === "consumed"), "consumed transfer " + type);
const runs = readJson("t3-runs.json");
check(runs.length === 8 && runs.every((x) => x.status === "completed"), "all eight T3 runs completed");
const bb = readJson("bb-fork-readback.json");
check(bb.id === "thr_pinm6gsbmv" && bb.sourceThreadId === "thr_sin3dht5et" && bb.originKind === "fork" && bb.status === "idle" && bb.parentThreadId === null, "BB persistent fork and idle readback");
for (const [name, sentinels] of [
  ["t3-roundtrip-and-merge.txt", ["text ORBIT-CEDAR-641 MAPLE-208", "text FORK-MERGE-517"]],
  ["t3-queue-result.txt", ["text WAIT_DONE", "text QUEUE_EDITED_OK"]],
  ["bb-eager-fork.txt", ["text SILVER-BIRCH-902"]],
]) {
  const text = fs.readFileSync(path.join(evidence, name), "utf8");
  for (const sentinel of sentinels) check(text.includes(sentinel), name + ": missing " + sentinel);
}
const queue = fs.readFileSync(path.join(evidence, "t3-queue-result.txt"), "utf8");
check(!/\btext QUEUE_FIRST(?:\r?\n|$)/.test(queue), "unedited queue sentinel did not become assistant output");
const images = fs.readdirSync(evidence).filter((n) => n.endsWith(".jpg"));
for (const name of images) {
  const bytes = fs.readFileSync(path.join(evidence, name));
  check(bytes[0] === 255 && bytes[1] === 216 && bytes.at(-2) === 255 && bytes.at(-1) === 217, "JPEG signature " + name);
}
const checksums = path.join(report, "SHA256SUMS");
if (fs.existsSync(checksums) && !process.argv.includes("--write")) {
  for (const line of fs.readFileSync(checksums, "utf8").trim().split("\n")) {
    const m = line.match(/^([a-f0-9]{64})  (.+)$/);
    check(Boolean(m), "checksum manifest syntax");
    if (m) check(sha(fs.readFileSync(path.join(report, m[2]))) === m[1], "checksum " + m[2]);
  }
}
const result = {
  ok: errors.length === 0,
  checkedAt: new Date().toISOString(),
  checkedLinks, checkedSourcePermalinks,
  sourceFingerprints: index.length,
  lunaCurrentSourceDrift: lunaDrift,
  imageArtifacts: images.length,
  recordedT3Runs: runs.length,
  freshProviderCallsMadeByVerifier: 0,
  upstreamTestSuitesRun: false,
  lunaBuildOrRuntimeTestRun: false,
  warnings, errors,
};
if (process.argv.includes("--write")) fs.writeFileSync(path.join(evidence, "validation.json"), JSON.stringify(result, null, 2) + "\n");
console.log(JSON.stringify(result, null, 2));
process.exitCode = result.ok ? 0 : 1;
