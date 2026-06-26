"use client";

/* The landing hero's signature moment: a living departure board. Three PR "cars"
   board onto the rail, stage + test as one batch, and either land into the green
   `main` terminus (the split-flap SHA flips) or — on the incident pass — one car
   trips DANGER and derails off the rule while the survivors re-couple and land.
   Every beat is driven off a real FSM state token via `data-rail`, never a
   decorative infinite loop: the rail only carries current while a batch is live. */

import { useEffect, useRef, useState } from "react";
import type { CSSProperties } from "react";

import { BatchState, EntryState } from "@/lib/api-types";
import { stateColor, stateInk, svar } from "@/lib/state";
import type { AnyState } from "@/lib/state";

type Rail = "idle" | "testing" | "merging" | "ejecting";

type Phase = {
  rail: Rail;
  cars: AnyState[];
  out: number | null;
  sha: string;
  status: string;
};

const CARS = [
  { n: 438, who: "@paul", ref: "feat/api" },
  { n: 440, who: "@lalith", ref: "fix/race" },
  { n: 442, who: "@michael", ref: "chore/deps" },
];

const SHA_A = "a1f9c2";
const SHA_B = "7d3e0b";
const SHA_C = "c4b18a";

const Q = EntryState.Queued;
const SG = BatchState.Staging;
const T = EntryState.Testing;
const M = EntryState.Merged;
const X = EntryState.Ejected;

const SCRIPT: Phase[] = [
  { rail: "idle", cars: [Q, Q, Q], out: null, sha: SHA_A, status: "boarding · batch 3" },
  { rail: "idle", cars: [SG, SG, SG], out: null, sha: SHA_A, status: "staging · mq/staging/main" },
  { rail: "testing", cars: [T, T, T], out: null, sha: SHA_A, status: "testing · ci running" },
  { rail: "merging", cars: [M, M, M], out: null, sha: SHA_B, status: "clear · fast-forward" },
  { rail: "idle", cars: [Q, Q, Q], out: null, sha: SHA_B, status: "boarding · batch 3" },
  { rail: "testing", cars: [T, T, T], out: null, sha: SHA_B, status: "testing · ci running" },
  { rail: "ejecting", cars: [M, X, M], out: 1, sha: SHA_B, status: "danger · bisecting" },
  { rail: "merging", cars: [M, X, M], out: 1, sha: SHA_C, status: "#440 ejected · rest land" },
];

const LAND_FRAME = 3;

/// A split-flap unit: the value mechanically flips (never fades) on change.
function useFlip(value: string) {
  const [shown, setShown] = useState(value);
  const [flipping, setFlipping] = useState(false);
  useEffect(() => {
    if (value === shown) return;
    setFlipping(true);
    const mid = window.setTimeout(() => setShown(value), 90);
    const end = window.setTimeout(() => setFlipping(false), 180);
    return () => {
      window.clearTimeout(mid);
      window.clearTimeout(end);
    };
  }, [value, shown]);
  return { shown, flipping };
}

function FlipTag({ value }: { value: AnyState }) {
  const { shown, flipping } = useFlip(value);
  return (
    <span
      className={`tag mini flip ${flipping ? "flipping" : ""}`}
      style={svar(stateColor[shown as AnyState], stateInk[shown as AnyState])}
    >
      <span className="flip-face">{shown}</span>
    </span>
  );
}

function FlipText({ value, className }: { value: string; className?: string }) {
  const { shown, flipping } = useFlip(value);
  return (
    <span className={`flip ${flipping ? "flipping" : ""} ${className ?? ""}`}>
      <span className="flip-face">{shown}</span>
    </span>
  );
}

export function DepartureBoard() {
  const [phase, setPhase] = useState(0);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) {
      setPhase(LAND_FRAME);
      return;
    }
    let inView = true;
    const el = ref.current;
    const io = new IntersectionObserver(
      ([entry]) => {
        inView = entry.isIntersecting;
      },
      { threshold: 0.25 },
    );
    if (el) io.observe(el);
    const id = window.setInterval(() => {
      if (!inView || document.hidden) return;
      setPhase((p) => (p + 1) % SCRIPT.length);
    }, 2300);
    return () => {
      window.clearInterval(id);
      io.disconnect();
    };
  }, []);

  const p = SCRIPT[phase];

  return (
    <div className="lp-board" data-rail={p.rail} aria-hidden ref={ref}>
      <div className="lp-board-head">
        <span className="lp-board-title">departures</span>
        <FlipText value={p.status} className="lp-board-status" />
      </div>

      <div className="lp-board-track">
        <span className="board-rail">
          <span className="board-surge" />
        </span>

        <div className="board-main">
          <span className="board-knob is-main" style={svar(stateColor[M])} />
          <span className="board-main-ref">main</span>
          <FlipText value={p.sha} className="board-main-sha" />
          <span className="board-main-lamp">always green</span>
        </div>

        <div className="board-bracket">
          <span className="board-bracket-k">next departure</span>
          <span className="board-bracket-rule" />
        </div>

        {CARS.map((c, i) => (
          <div
            className={`board-car ${p.out === i ? "is-out" : ""}`}
            key={c.n}
            style={{ "--i": i } as CSSProperties}
          >
            <span className="board-knob" style={svar(stateColor[p.cars[i]])} />
            <span className="board-car-num">#{c.n}</span>
            <span className="board-car-body">
              <span className="board-car-ref">
                {c.ref} <span className="board-car-arrow">→</span> main
              </span>
              <span className="board-car-who">{c.who}</span>
            </span>
            <FlipTag value={p.cars[i]} />
          </div>
        ))}
      </div>
    </div>
  );
}
