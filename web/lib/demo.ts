// Representative data for the dashboard until the store/API endpoints are wired.
// States are typed with the generated enums (no free strings).

import { BatchState, EntryState } from "./api-types";
import type { CheckState } from "./state";

export const REPO = { name: "withoneai/pica-v2", base: "main", batchSize: 3, mergeMethod: "squash" };
export const MAIN = { sha: "a1b2c3d", today: 9 };
export const INSTALL_URL = "https://github.com/apps/mergequeue/installations/new";

export const REPOS = [
  { name: "withoneai/pica-v2", queued: 3 },
  { name: "acme/api", queued: 0 },
  { name: "withoneai/robot", queued: 1 },
  { name: "withoneai/infrastructure", queued: 0 },
];

export type BatchPr = { num: number; title: string; author: string };
export type Check = { name: string; state: CheckState };

export const BATCH: { ref: string; state: BatchState; prs: BatchPr[]; checks: Check[] } = {
  ref: "mq/staging/main",
  state: BatchState.Testing,
  prs: [
    { num: 434, title: "feat: migrated zoom to new oauth", author: "sam" },
    { num: 436, title: "feat: migrated jira to new oauth", author: "jeff" },
  ],
  checks: [
    { name: "ci/woodpecker/woodpecker", state: "running" },
    { name: "composer-test", state: "pass" },
    { name: "oauth-test", state: "pass" },
  ],
};

export type QueuedPr = { num: number; title: string; author: string; state: EntryState };
export const QUEUE: QueuedPr[] = [
  { num: 438, title: "feat: migrate front oauth to native handler", author: "paul", state: EntryState.Queued },
  { num: 440, title: "feat: migrate google-ads oauth", author: "lalith", state: EntryState.Queued },
  { num: 442, title: "feat: migrated moneybird to new oauth", author: "michael", state: EntryState.Queued },
];

export type HistoryRow = { prs: number[]; result: EntryState.Merged | EntryState.Ejected; when: string };
export const HISTORY: HistoryRow[] = [
  { prs: [430, 425], result: EntryState.Merged, when: "4m" },
  { prs: [424], result: EntryState.Ejected, when: "22m" },
  { prs: [423, 422, 420], result: EntryState.Merged, when: "38m" },
];
