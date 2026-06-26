import type { Metadata } from "next";
import type { CSSProperties } from "react";
import Link from "next/link";

import { DepartureBoard } from "@/components/DepartureBoard";
import { InstallLine } from "@/components/InstallLine";
import { NavAuth } from "@/components/NavAuth";
import { ASSET_BASE, STATIC_SITE } from "@/lib/static-site";

import "./landing.css";

export const metadata: Metadata = {
  title: "mergequeue — the merge queue for any CI",
  description:
    "Batch ready PRs, test the combined result against the latest base, and land them together — or bisect to eject the one that broke. Self-hosted, works with any CI, on any plan.",
};

const GITHUB_URL = "https://github.com/LumaPrism/mergequeue";

const STEPS = [
  {
    k: "01",
    t: "Queue",
    d: "PRs board the train the moment they're approved and green — no babysitting, no manual rebases.",
  },
  {
    k: "02",
    t: "Batch",
    d: "The next N cars couple together onto the very latest base on a throwaway staging branch.",
  },
  {
    k: "03",
    t: "Test",
    d: "Your CI runs against the combined result — the exact code that's about to land.",
  },
  {
    k: "04",
    t: "Land — or eject",
    d: "Green fast-forwards base in one move. Red bisects to the breaker, ejects it, and re-couples the rest.",
  },
];

const SPEC = [
  {
    k: "ANY CI",
    t: "Works with any CI",
    d: "Woodpecker, GitHub Actions, Buildkite, CircleCI — mergequeue just reads your repo's required checks. No CI lock-in.",
  },
  {
    k: "SELF-HOSTED",
    t: "Self-hosted",
    d: "One small service plus Postgres. Your App keys and source never leave your infrastructure.",
  },
  {
    k: "ANY PLAN",
    t: "Any plan, private repos",
    d: "Runs on any GitHub plan, public or private repos. Install the App and go — nothing else to enable.",
  },
  {
    k: "BISECT-TO-EJECT",
    t: "Bisect to eject",
    d: "One bad PR never wedges the train. The batch is split until the culprit is found, ejected, and commented.",
  },
  {
    k: "CRASH-SAFE",
    t: "Crash-safe by design",
    d: "Every batch is an explicit, persisted FSM. State is written before each GitHub side effect, so a restart resumes mid-flight.",
  },
  {
    k: "OPTIMISTIC",
    t: "Optimistic batching",
    d: "Land several PRs per base move. Throughput scales with batch size while main stays always-green.",
  },
];

const COMPARE = [
  { f: "Works with any CI", mq: "yes", gh: "no", bors: "yes" },
  { f: "Self-hosted", mq: "yes", gh: "no", bors: "yes" },
  { f: "Private repos on any plan", mq: "yes", gh: "no", bors: "yes" },
  { f: "Bisect to eject the breaker", mq: "yes", gh: "no", bors: "part" },
  { f: "Crash-safe persisted FSM", mq: "yes", gh: "yes", bors: "no" },
  { f: "Maintained", mq: "yes", gh: "yes", bors: "no" },
];

const EJECT_LAMPS: { s: string; n: string }[] = [
  { s: "var(--st-merge)", n: "#438" },
  { s: "var(--st-merge)", n: "#439" },
  { s: "var(--st-eject)", n: "#440" },
  { s: "var(--st-merge)", n: "#441" },
  { s: "var(--st-merge)", n: "#442" },
];

const FSM_LAMPS: { s: string; t: string }[] = [
  { s: "var(--st-queued)", t: "queued" },
  { s: "var(--st-staging)", t: "staging" },
  { s: "var(--st-testing)", t: "testing" },
  { s: "var(--st-merge)", t: "merging" },
  { s: "var(--st-merge)", t: "merged" },
];

function GithubMark() {
  return (
    <svg viewBox="0 0 16 16" width="16" height="16" fill="currentColor" aria-hidden focusable="false">
      <path d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27.68 0 1.36.09 2 .27 1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.01 8.01 0 0016 8c0-4.42-3.58-8-8-8z" />
    </svg>
  );
}

export default function Landing() {
  return (
    <main className="lp">
      <nav className="lp-nav">
        <Link href="/" className="lp-brand">
          {/* eslint-disable-next-line @next/next/no-img-element */}
          <img src={`${ASSET_BASE}/logo.png`} alt="" className="lp-mark" />
          <span>
            merge<b>queue</b>
          </span>
        </Link>
        <div className="lp-nav-links">
          <Link href="/docs">Docs</Link>
          {!STATIC_SITE && <Link href="/app">Dashboard</Link>}
          <a
            className="lp-icon-link"
            href={GITHUB_URL}
            target="_blank"
            rel="noreferrer"
            aria-label="mergequeue on GitHub"
          >
            <GithubMark />
          </a>
          {!STATIC_SITE && <NavAuth />}
        </div>
      </nav>

      <header className="lp-hero">
        <div className="lp-hero-copy">
          <span className="lp-eyebrow">
            <span className="lp-eyebrow-dot" /> signal box · any ci
          </span>
          <h1>
            Never merge a<br />
            <span className="lp-accent">broken main</span> again.
          </h1>
          <p className="lp-lede">
            mergequeue batches your ready PRs, tests the combined result against the latest base, and
            lands them together — or bisects to eject the one that broke. Works with any CI.
            Self-hosted. Any plan.
          </p>

          <div className="lp-install">
            <InstallLine />
            <div className="lp-cta-row">
              <a className="btn btn-primary lp-star" href={GITHUB_URL} target="_blank" rel="noreferrer">
                <span className="lp-star-glyph" aria-hidden>
                  ★
                </span>
                Star on GitHub
              </a>
              {STATIC_SITE ? (
                <Link href="/docs" className="btn btn-ghost">
                  Open the docs
                  <span className="btn-arrow">→</span>
                </Link>
              ) : (
                <Link href="/app" className="btn btn-ghost">
                  Open the dashboard
                  <span className="btn-arrow">→</span>
                </Link>
              )}
            </div>
          </div>

          <p className="lp-sub">batch · test · land — or eject</p>
        </div>

        <DepartureBoard />
      </header>

      <section className="lp-how">
        <div className="lp-section-head">
          <h2>One queue, four moves.</h2>
          <p>From approved to landed without a human in the loop — and without a red main.</p>
        </div>
        <ol className="lp-steps">
          {STEPS.map((s) => (
            <li className="lp-step" key={s.k}>
              <span className="lp-step-k">
                <span className="lp-step-dot" />
                {s.k}
              </span>
              <h3>{s.t}</h3>
              <p>{s.d}</p>
            </li>
          ))}
        </ol>
      </section>

      <section className="lp-signature">
        <div className="lp-section-head">
          <h2>The two moves nobody else makes.</h2>
          <p>A wedged queue and a half-merged restart are the failure modes that bite. Both are designed out.</p>
        </div>
        <div className="lp-sig-grid">
          <article className="lp-sig-card">
            <span className="lp-sig-k">bisect to eject</span>
            <div className="lp-sig-diagram" aria-hidden>
              {EJECT_LAMPS.map((l) => (
                <span
                  className={`lp-lamp ${l.s.includes("eject") ? "is-out" : ""}`}
                  key={l.n}
                  style={{ "--s": l.s } as CSSProperties}
                >
                  <span className="lp-lamp-dot" />
                  <span className="lp-lamp-n">{l.n}</span>
                </span>
              ))}
            </div>
            <h3>One bad PR never wedges the train.</h3>
            <p>
              When the combined batch fails, mergequeue splits it and re-tests until the breaker is
              isolated. It derails off the line, gets ejected with a comment, and the survivors
              re-couple and land. No babysitting, no manual rebases.
            </p>
          </article>

          <article className="lp-sig-card">
            <span className="lp-sig-k">crash-safe by design</span>
            <div className="lp-sig-diagram lp-sig-fsm" aria-hidden>
              {FSM_LAMPS.map((l, i) => (
                <span className="lp-fsm-step" key={l.t} style={{ "--s": l.s } as CSSProperties}>
                  <span className="lp-lamp-dot" />
                  <span className="lp-fsm-t">{l.t}</span>
                  {i < FSM_LAMPS.length - 1 && <span className="lp-fsm-link" />}
                </span>
              ))}
              <span className="lp-fsm-resume">resume mid-flight ↺</span>
            </div>
            <h3>A restart picks up exactly where it left off.</h3>
            <p>
              Every batch is an explicit, persisted state machine. State is written before each
              GitHub side effect, so a redeploy or a crash mid-merge can&apos;t double-push or wedge a
              branch — the worker reads the FSM and continues.
            </p>
          </article>
        </div>
      </section>

      <section className="lp-spec">
        <div className="lp-section-head">
          <h2>Built to land code, not babysit it.</h2>
          <p>Instrument-grade specs — every guarantee, spelled out.</p>
        </div>
        <dl className="lp-spec-table">
          {SPEC.map((s) => (
            <div className="lp-spec-row" key={s.k}>
              <dt className="lp-spec-k">{s.k}</dt>
              <dd className="lp-spec-v">
                <b>{s.t}</b>
                <span>{s.d}</span>
              </dd>
            </div>
          ))}
        </dl>
      </section>

      <section className="lp-compare">
        <div className="lp-section-head">
          <h2>Where mergequeue wins.</h2>
          <p>The wedge: any CI, self-hosted, private repos on any plan.</p>
        </div>
        <div className="lp-compare-table" role="table">
          <div className="lp-compare-head" role="row">
            <span className="lp-compare-feat" role="columnheader" />
            <span className="lp-compare-col is-us" role="columnheader">
              mergequeue
            </span>
            <span className="lp-compare-col" role="columnheader">
              GitHub native
            </span>
            <span className="lp-compare-col" role="columnheader">
              bors
            </span>
          </div>
          {COMPARE.map((r) => (
            <div className="lp-compare-row" role="row" key={r.f}>
              <span className="lp-compare-feat" role="cell">
                {r.f}
              </span>
              <Cell v={r.mq} highlight />
              <Cell v={r.gh} />
              <Cell v={r.bors} />
            </div>
          ))}
        </div>
      </section>

      <section className="lp-band">
        <h2>
          Install the App. Point it at a repo.<br />
          <span className="lp-accent">Stop watching CI.</span>
        </h2>
        <div className="lp-cta-row">
          <a className="btn btn-primary" href={GITHUB_URL} target="_blank" rel="noreferrer">
            Install the App
            <span className="btn-arrow">→</span>
          </a>
          <Link href="/docs" className="btn btn-ghost">
            How it works
          </Link>
        </div>
      </section>

      <footer className="lp-foot">
        <span className="lp-foot-brand">
          merge<b>queue</b>
        </span>
        <span className="lp-foot-tag">ci-agnostic, self-hosted merge queue</span>
        <span className="lp-foot-links">
          <Link href="/docs">Docs</Link>
          {!STATIC_SITE && <Link href="/app">Dashboard</Link>}
          <a
            className="lp-icon-link"
            href={GITHUB_URL}
            target="_blank"
            rel="noreferrer"
            aria-label="mergequeue on GitHub"
          >
            <GithubMark />
          </a>
        </span>
      </footer>
    </main>
  );
}

function Cell({ v, highlight = false }: { v: string; highlight?: boolean }) {
  const label = v === "yes" ? "yes" : v === "no" ? "no" : "partial";
  return (
    <span className={`lp-compare-col lp-cell ${highlight ? "is-us" : ""}`} role="cell">
      <span className={`lp-cell-mark is-${v}`} aria-hidden>
        {v === "yes" ? "✓" : v === "no" ? "✕" : "~"}
      </span>
      <span className="lp-cell-label">{label}</span>
    </span>
  );
}
