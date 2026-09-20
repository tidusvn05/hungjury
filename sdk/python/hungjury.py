"""Thin Python wrapper over the `hungjury` CLI.

`system_one(state, questions)` keeps the shape of a fast typed-answer
call — under the hood it spawns `hungjury decide` once (jury of CLI
agents, optional judge escalation, local memory). Latency is seconds,
not milliseconds: use it for batch/CI/triage, not request-time code.

Requires the `hungjury` binary on PATH (or pass `bin=`).
"""

from __future__ import annotations

import json
import subprocess
from dataclasses import dataclass
from typing import Any, Mapping, Optional, Sequence


class HungjuryError(RuntimeError):
    """The CLI exited 1 (real error — parse, quota, no valid juror)."""


@dataclass
class Decision:
    """Parsed `decide` response.

    `hung` lists question keys left unresolved; `exit_code == 2` means
    the jury hung and escalation didn't resolve it (handle upstream —
    route to a human, retry with more jurors, ...).
    """

    id: str
    decided_by: str
    answers: dict[str, Any]
    hung: list[str]
    memory: dict[str, Any]
    usage: dict[str, Any]
    exit_code: int
    raw: dict[str, Any]

    @property
    def ok(self) -> bool:
        return self.exit_code == 0


def decide(
    state: Any,
    questions: Mapping[str, Any],
    *,
    jurors: Optional[Sequence[str]] = None,
    judge: Optional[str] = None,
    escalate: Optional[str] = None,
    hung_threshold: Optional[float] = None,
    policy_file: Optional[str] = None,
    namespace: Optional[str] = None,
    workspace: Optional[str] = None,
    hint: Optional[str] = None,
    extra_args: Sequence[str] = (),
    bin: str = "hungjury",
    cwd: Optional[str] = None,
    timeout: Optional[float] = None,
) -> Decision:
    """Run one decision.

    `state` is a plain string (text case) — or pass `workspace=` +
    `hint=` for a repository state. `questions` maps key →
    `{"type": "choice"|"score"|"noul", ...}` as in the CLI request JSON.

    `cwd` sets the subprocess working directory — hungjury walks up
    from it looking for `.hungjury/` (project config, policy, memory),
    so pass your project dir to pick up project memory.
    """
    if workspace is not None:
        state_field: Any = {"workspace": workspace, "hint": hint}
    else:
        state_field = state
    request = {"state": state_field, "questions": dict(questions)}

    args = [bin]
    if jurors is not None:
        args += ["--jurors", ",".join(jurors)]
    if judge is not None:
        args += ["--judge", judge]
    if escalate is not None:
        args += ["--escalate", escalate]
    if hung_threshold is not None:
        args += ["--hung-threshold", str(hung_threshold)]
    if policy_file is not None:
        args += ["--policy-file", policy_file]
    if namespace is not None:
        args += ["--namespace", namespace]
    args += list(extra_args)
    args += ["decide", "-"]

    proc = subprocess.run(
        args,
        input=json.dumps(request),
        capture_output=True,
        text=True,
        cwd=cwd,
        timeout=timeout,
    )
    # Exit 2 is a valid hung response — stdout still carries the JSON.
    if proc.returncode == 1 or not proc.stdout.strip():
        raise HungjuryError(proc.stderr.strip() or f"exit {proc.returncode}")
    raw = json.loads(proc.stdout)
    return Decision(
        id=raw["id"],
        decided_by=raw["decided_by"],
        answers=raw["answers"],
        hung=raw.get("hung", []),
        memory=raw.get("memory", {}),
        usage=raw.get("usage", {}),
        exit_code=proc.returncode,
        raw=raw,
    )


# PLAN calls the product shape `system_one(state, questions)`.
system_one = decide


def feedback(
    decision_id: str,
    sets: Mapping[str, Any],
    *,
    note: Optional[str] = None,
    bin: str = "hungjury",
    cwd: Optional[str] = None,
) -> dict[str, Any]:
    """Attach a human verdict to a past decision (`hungjury feedback`)."""
    args = [bin, "feedback", decision_id]
    for k, v in sets.items():
        args += ["--set", f"{k}={json.dumps(v)}"]
    if note:
        args += ["--note", note]
    proc = subprocess.run(args, capture_output=True, text=True, cwd=cwd)
    if proc.returncode != 0:
        raise HungjuryError(proc.stderr.strip() or f"exit {proc.returncode}")
    return json.loads(proc.stdout)
