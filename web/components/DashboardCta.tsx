"use client";

/* Hero secondary CTA on the live (non-static) landing. Defaults to the dashboard
   and falls back to the docs — the same neutral destination the static export uses —
   when the App is known-unregistered OR the setup-status request failed (a setup/key
   problem the backend reports; the dashboard would dead-end either way). Only while
   the status is still LOADING does it keep the dashboard link, so the main entry
   point isn't briefly lost; once a failure is known it stops pointing there. */

import Link from "next/link";

import { ASSET_BASE } from "@/lib/static-site";
import { useLandingAuth } from "@/lib/use-landing-auth";

export function DashboardCta() {
  const { setup, setupError } = useLandingAuth();

  if (setupError || (setup && !setup.registered)) {
    return (
      <a href={`${ASSET_BASE}/docs`} className="btn btn-ghost">
        Open the docs
        <span className="btn-arrow">→</span>
      </a>
    );
  }
  return (
    <Link href="/app" className="btn btn-ghost">
      Open the dashboard
      <span className="btn-arrow">→</span>
    </Link>
  );
}
