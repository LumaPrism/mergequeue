// Client for the mergequeue backend. Types are generated from the Rust backend
// by `just regen-types` (see lib/api-types.ts) — never hand-write them here.

import type {
  EntryView,
  EnqueueRequest,
  MeView,
  PrView,
  ReorderRequest,
  RepoView,
  SetupStatus,
} from "./api-types";

export type {
  EntryView,
  EnqueueRequest,
  MeView,
  PrView,
  ReorderRequest,
  RepoView,
  SetupStatus,
} from "./api-types";
export { EntryState, BatchState, MergeMethod, SetupSource } from "./api-types";

// Server components fetch the backend directly (absolute); client-side calls use
// the same-origin "/api" rewrite (see next.config.mjs).
const base =
  typeof window === "undefined"
    ? `${process.env.MQ_BACKEND_URL ?? "http://localhost:8080"}/api`
    : "/api";

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

export async function getRepos(): Promise<RepoView[]> {
  const res = await fetch(`${base}/repos`, { cache: "no-store" });
  if (!res.ok) throw new Error(`getRepos failed: ${res.status}`);
  return res.json();
}

/// Open PRs for a repo — candidates to add to the queue.
export async function getOpenPrs(repoId: string): Promise<PrView[]> {
  const res = await fetch(`${base}/repos/${repoId}/prs`, { cache: "no-store" });
  if (!res.ok) throw new Error(`getOpenPrs failed: ${res.status}`);
  return res.json();
}

export async function getQueue(repoId: string): Promise<EntryView[]> {
  const res = await fetch(`${base}/repos/${repoId}/queue`, { cache: "no-store" });
  if (!res.ok) throw new Error(`getQueue failed: ${res.status}`);
  return res.json();
}

export async function enqueue(repoId: string, prNumber: number): Promise<EntryView> {
  const body: EnqueueRequest = { prNumber };
  const res = await fetch(`${base}/repos/${repoId}/queue`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
  if (!res.ok) throw new Error(`enqueue failed: ${res.status}`);
  return res.json();
}

/// Reorder the queued train cars; returns the queue in its new order.
export async function reorder(repoId: string, entryIds: string[]): Promise<EntryView[]> {
  const body: ReorderRequest = { entryIds };
  const res = await fetch(`${base}/repos/${repoId}/queue/order`, {
    method: "PUT",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
  if (!res.ok) throw new Error(`reorder failed: ${res.status}`);
  return res.json();
}

/// Remove a car from the train.
export async function dequeue(repoId: string, entryId: string): Promise<void> {
  const res = await fetch(`${base}/repos/${repoId}/queue/${entryId}`, { method: "DELETE" });
  if (!res.ok) throw new Error(`dequeue failed: ${res.status}`);
}

// TODO: listOpenPrs filtering, getBatches — match api/mod.rs as it fills in.
