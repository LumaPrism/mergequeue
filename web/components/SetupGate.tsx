"use client";

/* Gates the GitHub App manifest flow on setup state: a connect CTA while the App
   is missing, a slim connected ribbon once it's registered. Reads /api/setup/status
   (typed; see lib/api-types.ts). Renders nothing until resolved, so it never flashes. */

import { useEffect, useState } from "react";

import { getSetupStatus, type SetupStatus } from "@/lib/api";

export function SetupGate() {
  const [status, setStatus] = useState<SetupStatus | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    let alive = true;
    getSetupStatus()
      .then((s) => alive && setStatus(s))
      .catch(() => alive && setFailed(true));
    return () => {
      alive = false;
    };
  }, []);

  if (failed || !status) return null;

  if (status.registered) {
    return (
      <div className="connected" role="status">
        <span className="ok" aria-hidden />
        <span className="ctext">
          GitHub App connected
          {status.slug ? (
            <>
              {" · "}
              <b>@{status.slug}</b>
            </>
          ) : null}
        </span>
        {status.installUrl ? (
          <a className="clink" href={status.installUrl}>
            install on a repo
          </a>
        ) : null}
        {status.manageUrl ? (
          <a className="clink ghost" href={status.manageUrl} target="_blank" rel="noreferrer">
            manage
          </a>
        ) : null}
      </div>
    );
  }

  return (
    <section className="gate" aria-label="connect mergequeue to github">
      <div className="gate-mark" aria-hidden>
        <span className="gate-pulse" />
      </div>
      <div className="gate-copy">
        <h2>Connect mergequeue to GitHub</h2>
        <p>
          Register the GitHub App and we mint the credentials, signing key, and webhook for you —
          one click, no manual setup. Keys are stored on your own server.
        </p>
      </div>
      <a className="gate-cta" href={status.setupUrl}>
        Register the GitHub App
        <span className="gate-arrow" aria-hidden>
          →
        </span>
      </a>
    </section>
  );
}
