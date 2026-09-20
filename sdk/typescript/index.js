"use strict";
// Thin zero-dependency wrapper over the `hungjury` CLI.
//
// `systemOne(state, questions)` keeps the shape of a fast typed-answer
// call — under the hood it spawns `hungjury decide` once (jury of CLI
// agents, optional judge escalation, local memory). Latency is seconds,
// not milliseconds: use it for batch/CI/triage, not request-time code.
//
// Requires the `hungjury` binary on PATH (or pass `bin`).

const { spawnSync } = require("node:child_process");

class HungjuryError extends Error {
  /** The CLI exited 1 (real error — parse, quota, no valid juror). */
  constructor(message, exitCode) {
    super(message);
    this.name = "HungjuryError";
    this.exitCode = exitCode;
  }
}

/**
 * Run one decision.
 *
 * @param {string} state — text case. For a repository state pass
 *   `opts.workspace` (+ `opts.hint`).
 * @param {object} questions — key → {type: "choice"|"score"|"noul", ...}
 *   as in the CLI request JSON.
 * @param {object} [opts]
 * @param {string[]} [opts.jurors]
 * @param {string} [opts.judge]
 * @param {string} [opts.escalate] — "sync"|"queue"|"off"
 * @param {number} [opts.hungThreshold]
 * @param {string} [opts.policyFile]
 * @param {string} [opts.namespace]
 * @param {string} [opts.workspace]
 * @param {string} [opts.hint]
 * @param {string[]} [opts.extraArgs]
 * @param {string} [opts.bin="hungjury"]
 * @param {string} [opts.cwd] — subprocess cwd; hungjury walks up from it
 *   looking for `.hungjury/`, so pass your project dir for project memory.
 * @param {number} [opts.timeout] — ms
 * @returns {{id:string, decidedBy:string, answers:object, hung:string[],
 *   sources:object, escalated:string[], memory:object, usage:object,
 *   exitCode:number, ok:boolean, raw:object}}
 */
function decide(state, questions, opts = {}) {
  const stateField =
    opts.workspace != null
      ? { workspace: opts.workspace, hint: opts.hint }
      : state;
  const request = { state: stateField, questions };

  const args = [opts.bin ?? "hungjury"];
  if (opts.jurors != null) args.push("--jurors", opts.jurors.join(","));
  if (opts.judge != null) args.push("--judge", opts.judge);
  if (opts.escalate != null) args.push("--escalate", opts.escalate);
  if (opts.hungThreshold != null)
    args.push("--hung-threshold", String(opts.hungThreshold));
  if (opts.policyFile != null) args.push("--policy-file", opts.policyFile);
  if (opts.namespace != null) args.push("--namespace", opts.namespace);
  for (const a of opts.extraArgs ?? []) args.push(a);
  args.push("decide", "-");

  const proc = spawnSync(args[0], args.slice(1), {
    input: JSON.stringify(request),
    encoding: "utf8",
    cwd: opts.cwd,
    timeout: opts.timeout,
    maxBuffer: 64 * 1024 * 1024,
  });
  if (proc.error) throw new HungjuryError(String(proc.error), -1);
  // Exit 2 is a valid hung response — stdout still carries the JSON.
  // Any other nonzero exit (crash, signal, unknown code) is an error.
  if (
    proc.status !== 0 &&
    proc.status !== 2
  ) {
    throw new HungjuryError(
      (proc.stderr || "").trim() || `exit ${proc.status}`,
      proc.status ?? -1,
    );
  }
  let raw;
  try {
    raw = JSON.parse(proc.stdout);
  } catch {
    throw new HungjuryError(
      `invalid JSON on stdout (exit ${proc.status}): ${proc.stdout.slice(0, 200)}`,
      proc.status ?? -1,
    );
  }
  const exitCode = proc.status ?? 0;
  return {
    id: raw.id,
    decidedBy: raw.decided_by,
    answers: raw.answers,
    hung: raw.hung ?? [],
    sources: raw.sources ?? {},
    escalated: raw.escalated ?? [],
    memory: raw.memory ?? {},
    usage: raw.usage ?? {},
    exitCode,
    ok: exitCode === 0,
    raw,
  };
}

// PLAN calls the product shape `system_one(state, questions)`.
const systemOne = decide;

/**
 * Attach a human verdict to a past decision (`hungjury feedback`).
 * @param {string} decisionId
 * @param {object} sets — key → verdict value
 * @param {object} [opts] — {note?, bin?, cwd?}
 */
function feedback(decisionId, sets, opts = {}) {
  const args = [opts.bin ?? "hungjury", "feedback", decisionId];
  for (const [k, v] of Object.entries(sets)) {
    args.push("--set", `${k}=${JSON.stringify(v)}`);
  }
  if (opts.note) args.push("--note", opts.note);
  const proc = spawnSync(args[0], args.slice(1), {
    encoding: "utf8",
    cwd: opts.cwd,
  });
  if (proc.error) throw new HungjuryError(String(proc.error), -1);
  if (proc.status !== 0) {
    throw new HungjuryError(
      (proc.stderr || "").trim() || `exit ${proc.status}`,
      proc.status ?? -1,
    );
  }
  return JSON.parse(proc.stdout);
}

module.exports = { decide, systemOne, feedback, HungjuryError };
