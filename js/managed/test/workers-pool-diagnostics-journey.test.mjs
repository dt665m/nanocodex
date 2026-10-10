// Journey for test/workers-pool-diagnostics.ts: a workers-pool file that passes
// but never reports "testfileFinished" (#951) must be named while the run is
// stalled, and the diagnostic must not end, pass or retry that run.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const managed = fileURLToPath(new URL("..", import.meta.url));
const vitest = fileURLToPath(new URL("../node_modules/vitest/vitest.mjs", import.meta.url));
const FIXTURE = "test-fixtures/workers-pool-stall/socket-close-in-flight.test.ts";

test("names the stalled workers-pool file and leaves the run stalled", async () => {
  const child = spawn(process.execPath, [vitest, "run", "--config", "vitest.pool-stall.config.ts"], {
    cwd: managed,
    env: { ...process.env, NANOCODEX_WORKERS_POOL_STALL_MS: "5000", NO_COLOR: "1" },
    detached: true,
    stdio: ["ignore", "pipe", "pipe"],
  });
  let output = "";
  let exited = null;
  child.stdout.on("data", chunk => { output += chunk; });
  child.stderr.on("data", chunk => { output += chunk; });
  child.on("exit", (code, signal) => { exited = { code, signal }; });
  const plain = () => output.replace(/\u001b\[[0-9;]*m/g, "");
  const tail = () => plain().slice(-4000);
  try {
    // This bounds the journey itself; the diagnostic under test has no deadline.
    const deadline = Date.now() + 120_000;
    const seen = () => plain().includes("✓ " + FIXTURE) && plain().includes("[workers-pool-diagnostics] " + FIXTURE);
    while (!seen() && !exited && Date.now() < deadline) await new Promise(resolve => setTimeout(resolve, 200));
    assert.equal(exited, null, "vitest exited instead of stalling:\n" + tail());
    assert.ok(plain().includes("✓ " + FIXTURE), "fixture did not pass:\n" + tail());
    assert.ok(plain().includes("[workers-pool-diagnostics] " + FIXTURE + " has not reported testfileFinished"), "no diagnostic named the stalled file:\n" + tail());
    // Still stalled after the report: the process keeps running and prints no summary.
    await new Promise(resolve => setTimeout(resolve, 2_000));
    assert.equal(exited, null, "vitest exited after the diagnostic:\n" + tail());
    assert.doesNotMatch(plain(), /Test Files/);
    const log = /Log: (\S+\.log)/.exec(plain())?.[1];
    assert.ok(log, "diagnostic did not name its log file:\n" + tail());
    assert.ok(readFileSync(log, "utf8").includes(FIXTURE));
    console.log(plain().split("\n").filter(line => line.includes("[workers-pool-diagnostics]")).join("\n"));
  } finally {
    if (!exited) process.kill(-child.pid, "SIGKILL");
  }
});
