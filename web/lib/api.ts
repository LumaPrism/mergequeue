// Client for the mergequeue backend. Types are generated from the Rust backend
// by `just regen-types` (see lib/api-types.ts) — never hand-write them here.

import type {
  CreateQueueRequest,
  EntryView,
  EnqueueRequest,
  LedgerView,
  MeView,
  PrView,
  QueueView,
  ReorderRequest,
  RepoView,
  SetupStatus,
} from "./api-types";

export type {
  ActiveBatchView,
  CreateQueueRequest,
  EntryView,
  EnqueueRequest,
  LedgerView,
  MeView,
  PrView,
  QueueView,
  ReorderRequest,
  RepoView,
  SetupStatus,
} from "./api-types";
export { EntryState, BatchState, LedgerEntryResult, LedgerOutcome, MergeMethod, PrStatus, SetupSource } from "./api-types";

// Server components fetch the backend directly (absolute); client-side calls use
// the same-origin "/api" rewrite (see next.config.mjs).
const base =
  typeof window === "undefined"
    ? `${process.env.MQ_BACKEND_URL ?? "http://localhost:8080"}/api`
    : "/api";

/// An error from a backend call that carries the HTTP `status` alongside the
/// backend's human-readable message. The backend (poem) renders its rejections as
/// a plain-text body, so callers can show that text verbatim for user-fixable
/// failures (409/422) and fall back to a transient message for 5xx.
export class ApiError extends Error {
  readonly status: number;

  constructor(status: number, message: string) {
    super(message);
    this.name = "ApiError";
    this.status = status;
  }

  /// A 4xx the user can act on (wrong base, already queued, name taken) — its
  /// message is meant to be shown verbatim. 5xx/0 are transient/opaque.
  get userFixable(): boolean {
    return this.status >= 400 && this.status < 500 && this.message.length > 0;
  }
}

/// Build an `ApiError` from a non-OK response, reading the plain-text body as the
/// message and falling back to a status-tagged label when the body is empty.
async function apiError(res: Response, label: string): Promise<ApiError> {
  let body = "";
  try {
    body = (await res.text()).trim();
  } catch {
    body = "";
  }
  return new ApiError(res.status, body || `${label} failed: ${res.status}`);
}

export async function getSetupStatus(): Promise<SetupStatus> {
  const res = await fetch(`${base}/setup/status`, { cache: "no-store" });
  if (!res.ok) throw new Error(`getSetupStatus failed: ${res.status}`);
  return res.json();
}

/// The signed-in user, or null when unauthenticated (401).
export async function getMe(): Promise<MeView | null> {
  const res = await fetch(`${base}/me`, { cache: "no-store" });
  if (res.status === 401) return null;
  if (!res.ok) throw new Error(`getMe failed: ${res.status}`);
  return res.json();
}

/// The managed repos, each carrying its named queues (the dashboard switcher).
export async function getRepos(): Promise<RepoView[]> {
  const res = await fetch(`${base}/repos`, { cache: "no-store" });
  if (!res.ok) throw new Error(`getRepos failed: ${res.status}`);
  return res.json();
}

/// A repo's named queues with live depth + active-batch summary (the platform
/// switcher's source of truth — `active` populates each track's aspect pip).
export async function getQueues(repoId: string): Promise<QueueView[]> {
  const res = await fetch(`${base}/repos/${repoId}/queues`, { cache: "no-store" });
  if (!res.ok) throw new Error(`getQueues failed: ${res.status}`);
  return res.json();
}

/// Create a named queue (track) on a repo; config defaults from its `default`
/// queue. Returns the new queue's view.
export async function createQueue(
  repoId: string,
  body: CreateQueueRequest,
): Promise<QueueView> {
  const res = await fetch(`${base}/repos/${repoId}/queues`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
  if (!res.ok) throw await apiError(res, "createQueue");
  return res.json();
}

/// Open PRs for a repo — candidates to add to a queue.
export async function getOpenPrs(repoId: string): Promise<PrView[]> {
  const res = await fetch(`${base}/repos/${repoId}/prs`, { cache: "no-store" });
  if (!res.ok) throw new Error(`getOpenPrs failed: ${res.status}`);
  return res.json();
}

/// A queue's entries projected against its active batch (the train view).
export async function getQueue(queueId: string): Promise<EntryView[]> {
  const res = await fetch(`${base}/queues/${queueId}`, { cache: "no-store" });
  if (!res.ok) throw new Error(`getQueue failed: ${res.status}`);
  return res.json();
}

export async function enqueue(queueId: string, prNumber: number): Promise<EntryView> {
  const body: EnqueueRequest = { prNumber };
  const res = await fetch(`${base}/queues/${queueId}/enqueue`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
  if (!res.ok) throw await apiError(res, "enqueue");
  return res.json();
}

/// Reorder the queued train cars; returns the queue in its new order.
export async function reorder(queueId: string, entryIds: string[]): Promise<EntryView[]> {
  const body: ReorderRequest = { entryIds };
  const res = await fetch(`${base}/queues/${queueId}/order`, {
    method: "PUT",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
  if (!res.ok) throw new Error(`reorder failed: ${res.status}`);
  return res.json();
}

/// Remove a car from the train.
export async function dequeue(queueId: string, entryId: string): Promise<void> {
  const res = await fetch(`${base}/queues/${queueId}/entries/${entryId}`, { method: "DELETE" });
  if (!res.ok) throw new Error(`dequeue failed: ${res.status}`);
}

/// A queue's finished batch runs, newest first (the departures board).
export async function getLedger(queueId: string): Promise<LedgerView[]> {
  const res = await fetch(`${base}/queues/${queueId}/ledger`, { cache: "no-store" });
  if (!res.ok) throw new Error(`getLedger failed: ${res.status}`);
  return res.json();
}

// TODO: listOpenPrs filtering, getBatches — match api/mod.rs as it fills in.
