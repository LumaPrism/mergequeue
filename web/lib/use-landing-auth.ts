"use client";

/* Shared landing-nav auth/setup state. The landing page is a server component with
   several client islands (nav auth slot, Dashboard links, hero CTA) that all need
   the same two answers: is someone signed in, and is the GitHub App registered yet.
   Each island calls this hook; the underlying fetches are deduped to a single
   getMe + getSetupStatus per page load via the module-level promise cache. */

import { useEffect, useState } from "react";

import { getMe, getSetupStatus } from "@/lib/api";
import type { MeView, SetupStatus } from "@/lib/api";

export type LandingAuth = {
  loaded: boolean;
  me: MeView | null;
  setup: SetupStatus | null;
  /** The setup-status request FAILED — the backend reported a setup/key problem
      (e.g. stored App secrets can't be decrypted: a missing or wrong MQ_SECRET__KEY).
      Distinct from a clean "not registered yet" (`setup.registered === false`); the
      login/dashboard would dead-end, so the UI must point at recovery, not hide it. */
  setupError: boolean;
  /** Convenience: the App exists, so login + dashboard are reachable. */
  registered: boolean;
};

type LandingData = { me: MeView | null; setup: SetupStatus | null; setupError: boolean };

let cached: Promise<LandingData> | null = null;

function load() {
  if (!cached) {
    cached = Promise.all([
      getMe().catch(() => null),
      getSetupStatus().then(
        (setup) => ({ setup, setupError: false }),
        () => ({ setup: null, setupError: true }),
      ),
    ]).then(([me, s]) => ({ me, setup: s.setup, setupError: s.setupError }));
  }
  return cached;
}

export function useLandingAuth(): LandingAuth {
  const [state, setState] = useState<{ loaded: boolean } & LandingData>({
    loaded: false,
    me: null,
    setup: null,
    setupError: false,
  });

  useEffect(() => {
    let alive = true;
    load().then((data) => alive && setState({ loaded: true, ...data }));
    return () => {
      alive = false;
    };
  }, []);

  return { ...state, registered: !!state.setup?.registered };
}
