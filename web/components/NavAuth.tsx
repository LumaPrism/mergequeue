"use client";

/* Landing-nav auth slot (live dashboard only — gated behind !STATIC_SITE at the
   call site, so it never mounts in the GitHub Pages export). Three states, in
   order: the signed-in handle; a "Set up" CTA when signed out *and the GitHub App
   isn't registered yet* (OAuth login is impossible without it, so we point at the
   manifest flow instead); and the plain "Log in with GitHub" once the App exists. */

import { useLandingAuth } from "@/lib/use-landing-auth";

export function NavAuth() {
  const { loaded, me, setup, registered, setupError } = useLandingAuth();

  if (!loaded) return null;
  if (me) {
    return (
      <a className="lp-user" href="/app" title="open the dashboard">
        @{me.login}
      </a>
    );
  }
  // Signed out and either the App isn't registered yet, or its stored secrets are
  // broken (a setup/key error the backend reports) — login would dead-end, so point
  // the operator at the setup flow rather than an unusable "Log in".
  if (setupError || (setup && !registered)) {
    return (
      <a className="lp-setup" href={setup?.setupUrl ?? "/setup"}>
        <span className="lp-setup-spark" aria-hidden>
          ✦
        </span>
        {setupError ? "Fix setup" : "Set up mergequeue"}
      </a>
    );
  }
  return (
    <a className="lp-login" href="/auth/github/login">
      Log in with GitHub
    </a>
  );
}
