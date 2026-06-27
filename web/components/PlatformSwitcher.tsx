"use client";

/* The platform board: a repo's named queues as railway TRACKS. Each chip carries
   a track marker, the queue name (board voice), its waiting depth, and a live
   aspect pip (the active batch's signal colour, grey when the track is clear).
   The selected track is lit amber. A trailing ghost "+ TRACK" chip opens a
   lightweight name input that creates a new track. Roving-tabindex keyboarding
   matches the repo switcher: ←/→ (and ↑/↓) move + select, Home/End jump. */

import { useRef, useState } from "react";
import type { CSSProperties } from "react";

import { ApiError } from "@/lib/api";
import type { QueueView } from "@/lib/api";
import { stateColor, svar } from "@/lib/state";

const NAME_RE = /^[a-z0-9][a-z0-9-]*$/;

interface PlatformSwitcherProps {
  queues: QueueView[];
  selected: string | null;
  onSelect: (queueId: string) => void;
  onCreate: (name: string) => Promise<void>;
}

/// The inline "+ TRACK" affordance — a ghost chip that becomes a name field.
function NewTrack({ onCreate }: { onCreate: (name: string) => Promise<void> }) {
  const [adding, setAdding] = useState(false);
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);

  const clean = name.trim().toLowerCase();
  const valid = NAME_RE.test(clean);

  const open = () => {
    setAdding(true);
    setErr(null);
    window.setTimeout(() => inputRef.current?.focus(), 0);
  };

  const close = () => {
    setAdding(false);
    setName("");
    setErr(null);
    setBusy(false);
  };

  const submit = async () => {
    if (!valid || busy) return;
    setBusy(true);
    setErr(null);
    try {
      await onCreate(clean);
      close();
    } catch (e) {
      // The backend explains a rejected name (e.g. 409 duplicate) — surface it
      // verbatim rather than swallowing every failure into one guess.
      setErr(
        e instanceof ApiError && e.userFixable
          ? e.message
          : "couldn’t open that track — name may be taken",
      );
      setBusy(false);
    }
  };

  if (!adding) {
    return (
      <button type="button" className="track track-new" onClick={open}>
        <span className="track-plus" aria-hidden>
          +
        </span>
        <span className="track-name">track</span>
      </button>
    );
  }

  return (
    <div className="track-form" role="group" aria-label="new track">
      <div className={`track track-input ${err ? "bad" : ""}`}>
        <span className="track-dot" aria-hidden />
        <input
          ref={inputRef}
          className="track-field"
          type="text"
          value={name}
          spellCheck={false}
          autoCapitalize="off"
          autoCorrect="off"
          placeholder="track name"
          aria-label="track name"
          aria-invalid={name.length > 0 && !valid}
          disabled={busy}
          onChange={(e) => setName(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              submit();
            } else if (e.key === "Escape") {
              e.preventDefault();
              close();
            }
          }}
        />
        <button
          type="button"
          className="track-go"
          aria-label="create track"
          disabled={!valid || busy}
          onClick={submit}
        >
          {busy ? "…" : "→"}
        </button>
      </div>
      <span className={`track-hint ${err ? "bad" : ""}`} role={err ? "alert" : undefined}>
        {err ?? (name.length > 0 && !valid ? "lowercase, digits, dashes" : "name the track")}
      </span>
    </div>
  );
}

export function PlatformSwitcher({ queues, selected, onSelect, onCreate }: PlatformSwitcherProps) {
  const tabRefs = useRef<(HTMLButtonElement | null)[]>([]);
  const selIdx = queues.findIndex((q) => q.id === selected);
  const rovingIdx = selIdx >= 0 ? selIdx : 0;

  const onKey = (e: React.KeyboardEvent, i: number) => {
    if (queues.length === 0) return;
    let next = i;
    if (e.key === "ArrowRight" || e.key === "ArrowDown") next = (i + 1) % queues.length;
    else if (e.key === "ArrowLeft" || e.key === "ArrowUp")
      next = (i - 1 + queues.length) % queues.length;
    else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = queues.length - 1;
    else if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      onSelect(queues[i].id);
      return;
    } else return;
    e.preventDefault();
    onSelect(queues[next].id);
    tabRefs.current[next]?.focus();
  };

  return (
    <section className="pswitch" aria-label="platforms">
      <span className="pswitch-lbl" aria-hidden>
        platforms
      </span>
      <div className="pswitch-rail" role="tablist" aria-label="queues">
        {queues.map((q, i) => {
          const on = q.id === selected;
          const live = q.active != null;
          const pip = live ? stateColor[q.active!.state] : "var(--st-queued)";
          const no = String(i + 1).padStart(2, "0");
          return (
            <button
              key={q.id}
              ref={(el) => {
                tabRefs.current[i] = el;
              }}
              type="button"
              role="tab"
              aria-selected={on}
              aria-label={`${q.name} track, ${q.depth} waiting${live ? ", batch running" : ""}`}
              tabIndex={i === rovingIdx ? 0 : -1}
              className={`track ${on ? "on" : ""}`}
              style={{ ["--i"]: i } as CSSProperties}
              onKeyDown={(e) => onKey(e, i)}
              onClick={() => onSelect(q.id)}
            >
              <span className="track-no" aria-hidden>
                {no}
              </span>
              <span className="track-name">{q.name}</span>
              <span className="track-depth" aria-hidden>
                <b>{q.depth}</b> waiting
              </span>
              <span
                className={`track-pip ${live ? "live" : ""}`}
                style={svar(pip)}
                aria-hidden
              />
            </button>
          );
        })}
      </div>
      <NewTrack onCreate={onCreate} />
    </section>
  );
}
