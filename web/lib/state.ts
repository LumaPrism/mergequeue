// Single source of truth for state → colour. Keyed off the typeshare-generated
// enums, so a backend state rename becomes a compile error here (no free
// strings). CheckState is a GitHub check conclusion, not a domain enum.

import type { CSSProperties } from "react";

import { BatchState, EntryState, PrStatus } from "./api-types";

export type AnyState = EntryState | BatchState;
export type CheckState = "pass" | "running" | "fail";

/** Every queue/batch state maps to a token (exhaustive — adding a variant breaks the build). */
export const stateColor: Record<AnyState, string> = {
  [EntryState.Queued]: "var(--st-queued)",
  [BatchState.Staging]: "var(--st-staging)",
  [EntryState.Testing]: "var(--st-testing)",
  [BatchState.Merging]: "var(--st-merge)",
  [EntryState.Merged]: "var(--st-merge)",
  [BatchState.Bisecting]: "var(--st-eject)",
  [EntryState.Ejected]: "var(--st-eject)",
  [BatchState.Superseded]: "var(--st-superseded)",
};

/**
 * Brighter, AA-on-dark *ink* for a state — used for tag/label TEXT, while
 * `stateColor` stays the core hue for fills, dots and borders. Exhaustive over
 * `AnyState`, so a backend state rename breaks the build here too.
 */
export const stateInk: Record<AnyState, string> = {
  [EntryState.Queued]: "var(--st-queued-ink)",
  [BatchState.Staging]: "var(--st-staging-ink)",
  [EntryState.Testing]: "var(--st-testing-ink)",
  [BatchState.Merging]: "var(--st-merge-ink)",
  [EntryState.Merged]: "var(--st-merge-ink)",
  [BatchState.Bisecting]: "var(--st-eject-ink)",
  [EntryState.Ejected]: "var(--st-eject-ink)",
  [BatchState.Superseded]: "var(--st-superseded-ink)",
};

/** The dashboard's per-PR display status → token (exhaustive over PrStatus). */
export const statusColor: Record<PrStatus, string> = {
  [PrStatus.Queued]: "var(--st-queued)",
  [PrStatus.Testing]: "var(--st-testing)",
  [PrStatus.Merging]: "var(--st-merge)",
  [PrStatus.Blocked]: "var(--st-blocked)",
  [PrStatus.Merged]: "var(--st-merge)",
  [PrStatus.Ejected]: "var(--st-eject)",
};

export const statusInk: Record<PrStatus, string> = {
  [PrStatus.Queued]: "var(--st-queued-ink)",
  [PrStatus.Testing]: "var(--st-testing-ink)",
  [PrStatus.Merging]: "var(--st-merge-ink)",
  [PrStatus.Blocked]: "var(--st-blocked-ink)",
  [PrStatus.Merged]: "var(--st-merge-ink)",
  [PrStatus.Ejected]: "var(--st-eject-ink)",
};

export const checkColor: Record<CheckState, string> = {
  pass: "var(--st-merge)",
  running: "var(--st-testing)",
  fail: "var(--st-eject)",
};

/**
 * Inline custom properties carrying a state's colour (`--s`) and, optionally, its
 * brighter text ink (`--s-ink`) to a styled element. Pass both for tags/labels so
 * uppercase microtext clears AA; pass just the core for dots/knobs/borders.
 */
export const svar = (color: string, ink?: string): CSSProperties =>
  ({ ["--s"]: color, ...(ink ? { ["--s-ink"]: ink } : {}) }) as CSSProperties;
