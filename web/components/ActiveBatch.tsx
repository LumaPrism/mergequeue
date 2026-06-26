"use client";

import type { EntryView, PrView } from "@/lib/api";
import { PrStatus } from "@/lib/api";
import { statusColor, statusInk, svar } from "@/lib/state";

type BatchStage = "testing" | "merging" | "merged" | "ejected" | null;

interface ActiveBatchProps {
  pinned: EntryView[];
  head: PrStatus | null;
  stage: BatchStage;
  prByNum: Map<number, PrView>;
  ghUrl: (n: number) => string;
  baseBranch?: string;
  onRemove: (e: EntryView) => void;
}

function pipeCls(step: "staging" | "testing" | "merging", stage: BatchStage): string {
  if (step === "staging") return stage ? "done" : "";
  if (step === "testing") {
    if (stage === "testing") return "active";
    if (stage === "merging" || stage === "merged") return "done";
    if (stage === "ejected") return "danger";
    return "";
  }
  if (stage === "merging") return "active";
  if (stage === "merged") return "done";
  return "";
}

const stop = (e: { stopPropagation: () => void }) => e.stopPropagation();

export function ActiveBatch({
  pinned,
  head,
  stage,
  prByNum,
  ghUrl,
  baseBranch,
  onRemove,
}: ActiveBatchProps) {
  if (!head || pinned.length === 0) {
    return (
      <div
        className="activebatch idle"
        style={svar(statusColor[PrStatus.Queued], statusInk[PrStatus.Queued])}
      >
        <div className="ab-banner">
          <span className="ab-state">IDLE</span>
          <span className="ab-count">no active batch</span>
        </div>
      </div>
    );
  }

  return (
    <div className="activebatch" style={svar(statusColor[head], statusInk[head])}>
      <div className="ab-banner">
        <span className="ab-state">{head.toUpperCase()}</span>
        <span className="ab-count">
          {pinned.length} {pinned.length === 1 ? "car" : "cars"}
        </span>
      </div>

      {head === PrStatus.Blocked && (
        <p className="ab-reason">
          checks passed — merge blocked by a branch ruleset; add the app as a bypass actor.
        </p>
      )}

      <div className="pipe ab-progress">
        <span className={`pill ${pipeCls("staging", stage)}`}>Staged</span>
        <span className={`arrow ${stage === "testing" ? "hot" : ""}`} />
        <span className={`pill ${pipeCls("testing", stage)}`}>Testing</span>
        <span className={`arrow ${stage === "merging" ? "hot" : ""}`} />
        <span className={`pill ${pipeCls("merging", stage)}`}>Merging</span>
        <span className="arrow" />
        <span className={`pill ${stage === "merged" ? "done" : ""}`}>Landed</span>
      </div>

      <div className="ab-prs">
        {pinned.map((e) => (
          <div key={e.id} className="ab-pr" style={svar(statusColor[e.status], statusInk[e.status])}>
            <span className="ab-pr-num">
              <a
                className="prlink"
                href={ghUrl(e.prNumber)}
                target="_blank"
                rel="noreferrer"
                onPointerDown={stop}
                onKeyDown={stop}
              >
                #{e.prNumber}
              </a>
            </span>
            <span className="ab-pr-ttl">
              {prByNum.get(e.prNumber)?.title ?? `PR #${e.prNumber}`}
            </span>
            <span className="tag">{e.status}</span>
            {(e.status === PrStatus.Testing ||
              e.status === PrStatus.Merging ||
              e.status === PrStatus.Blocked) && (
              <button
                type="button"
                className="car-x"
                aria-label={`remove #${e.prNumber}`}
                title="Remove — cancels the current batch"
                onClick={() => onRemove(e)}
              >
                ×
              </button>
            )}
          </div>
        ))}
      </div>

      {(head === PrStatus.Merged || head === PrStatus.Ejected) && (
        <div className="ab-result">
          <span className={`res ${head === PrStatus.Merged ? "merged" : "ejected"}`}>
            {head}
          </span>
          <span>
            {head === PrStatus.Merged
              ? `batch landed${baseBranch ? ` on ${baseBranch}` : ""}.`
              : "breaker isolated; survivors re-queue and roll on."}
          </span>
        </div>
      )}
    </div>
  );
}
