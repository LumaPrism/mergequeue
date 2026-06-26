import type { LedgerView } from "@/lib/api";
import { LedgerOutcome } from "@/lib/api";
import { relTime } from "@/lib/rel-time";

export function RunHistory({ rows }: { rows: LedgerView[] }) {
  const now = Date.now();

  return (
    <section className="history">
      <span className="history-title">recent runs</span>
      {rows.length === 0 ? (
        <p className="detail-sub">no runs yet</p>
      ) : (
        rows.map((row) => {
          const prs = row.entries.map((e) => `#${e.prNumber}`).join(" ");
          const tail =
            row.outcome === LedgerOutcome.Merged
              ? (row.landedSha?.slice(0, 7) ?? null)
              : row.outcome === LedgerOutcome.Ejected
                ? `ejected #${row.ejectedPr}`
                : null;
          return (
            <div key={row.id} className="hrow">
              <span className={`res ${row.outcome}`}>{row.outcome}</span>
              <span className="hrow-prs">{prs}</span>
              {tail ? <span className="hrow-tail">{tail}</span> : null}
              <span className="hrow-when">{relTime(new Date(row.endedAt).getTime(), now)}</span>
            </div>
          );
        })
      )}
    </section>
  );
}
