export interface Question {
  type: "choice" | "score" | "noul";
  id?: string;
  instructions?: string;
  criteria?: Record<string, string> | string[];
}

export interface DecideOptions {
  jurors?: string[];
  judge?: string;
  escalate?: "sync" | "queue" | "off";
  hungThreshold?: number;
  policyFile?: string;
  namespace?: string;
  workspace?: string;
  hint?: string;
  extraArgs?: string[];
  bin?: string;
  /** Subprocess cwd — hungjury walks up from it looking for `.hungjury/`. */
  cwd?: string;
  /** Milliseconds. */
  timeout?: number;
}

export interface Decision {
  id: string;
  decidedBy: string;
  answers: Record<string, unknown>;
  /** Question keys left unresolved (exit 2 when non-empty). */
  hung: string[];
  /** Per-key verdict source: "jury" | "judge" | "cache". */
  sources: Record<string, string>;
  /** Keys that were escalated to the judge. */
  escalated: string[];
  memory: Record<string, unknown>;
  usage: Record<string, unknown>;
  /** 0 decided · 1 error · 2 hung. */
  exitCode: number;
  ok: boolean;
  raw: Record<string, unknown>;
}

export declare class HungjuryError extends Error {
  exitCode: number;
}

export declare function decide(
  state: string,
  questions: Record<string, Question>,
  opts?: DecideOptions,
): Decision;

export declare const systemOne: typeof decide;

export declare function feedback(
  decisionId: string,
  sets: Record<string, unknown>,
  opts?: { note?: string; bin?: string; cwd?: string },
): Record<string, unknown>;
