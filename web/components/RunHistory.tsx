"use client";

/* The DEPARTURES board — run history promoted to a first-class surface. Each
   finished batch is a departure: when it left (relative time), the TRACK it ran
   on, the SERVICE it carried (the PR route), and its final ASPECT — a signal
   lamp reading DEPARTED (landed/green), FAILED (ejected/red) or CANCELLED
   (superseded/slate). Rows reveal with a staggered split-flap, the board's one
   canonical motion. */

import type { CSSProperties } from "react";

import type { LedgerView } from "@/lib/api";
import { LedgerOutcome } from "@/lib/api";
import { relTime } from "@/lib/rel-time";

type Aspect = { label: string; cls: string };

const ASPECT: Record<LedgerOutcome, Aspect> = {
  [LedgerOutcome.Merged]: { label: "departed", cls: "departed" },
  [LedgerOutcome.Ejected]: { label: "failed", cls: "failed" },
  [LedgerOutcome.Superseded]: { label: "cancelled", cls: "cancelled" },
};

interface DeparturesBoardProps {
  rows: LedgerView[];
  /// The selected track's name, shown in the TRACK column. The ledger view
  /// carries no queue identity, so the focal queue supplies it.
  track?: string;
  /// While the focal queue is loading, hold the board steady with placeholder
  /// rows rather than flashing the empty state.
  loading?: boolean;
}

export function DeparturesBoard({ rows, track, loading }: DeparturesBoardProps) {
  const now = Date.now();

  return (
    <section className="dep" aria-label="departures">
      <div className="dep-head">
        <span className="dep-live" aria-hidden />
        <span className="dep-title">departures</span>
        <span className="dep-sub" role="status" aria-live="polite">
          {loading ? "—" : rows.length === 0 ? "no service" : `${rows.length} logged`}
        </span>
      </div>

      <div className="dep-cols" aria-hidden>
        <span>time</span>
        <span>track</span>
        <span>service</span>
        <span>aspect</span>
      </div>

      {loading ? (
        <div className="dep-rows" aria-hidden>
          {Array.from({ length: 5 }).map((_, i) => (
            <div className="dep-row dep-skel" key={i} style={{ ["--i"]: i } as CSSProperties}>
              <span className="skel skel-bar" style={{ width: "60%" }} />
              <span className="skel skel-bar" style={{ width: "50%" }} />
              <span className="skel skel-bar" style={{ width: "40%" }} />
              <span className="skel skel-bar" style={{ width: "70%" }} />
            </div>
          ))}
        </div>
      ) : rows.length === 0 ? (
        <div className="dep-empty">
          <span className="dep-empty-lamp" aria-hidden />
          <span className="dep-empty-k">no departures</span>
          <span className="dep-empty-sub">
            Finished batches post here as they depart — green when they land, red when a breaker is
            ejected.
          </span>
        </div>
      ) : (
        <div className="dep-rows">
          {rows.map((row, i) => {
            const aspect = ASPECT[row.outcome];
            const service = row.entries.map((e) => `#${e.prNumber}`).join(" · ");
            return (
              <div key={row.id} className="dep-row" style={{ ["--i"]: i } as CSSProperties}>
                <span className="dep-time">{relTime(new Date(row.endedAt).getTime(), now)}</span>
                <span className="dep-track">{track ?? "—"}</span>
                <span className="dep-service" title={service}>
                  {row.entries.map((e, j) => (
                    <span key={e.prNumber}>
                      {j > 0 ? (
                        <span className="dep-sep" aria-hidden>
                          {" · "}
                        </span>
                      ) : null}
                      <span className="dep-pr">#{e.prNumber}</span>
                    </span>
                  ))}
                </span>
                <span className={`dep-aspect ${aspect.cls}`}>
                  <span className="dep-lamp" aria-hidden />
                  {aspect.label}
                </span>
              </div>
            );
          })}
        </div>
      )}
    </section>
  );
}
