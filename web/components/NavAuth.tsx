"use client";

/* Landing-nav auth slot: shows "Log in with GitHub" only when signed out, and the
   signed-in handle otherwise — so a logged-in visitor never sees a login button. */

import { useEffect, useState } from "react";

import { getMe } from "@/lib/api";
import type { MeView } from "@/lib/api";

export function NavAuth() {
  const [me, setMe] = useState<MeView | null>(null);
  const [loaded, setLoaded] = useState(false);

  useEffect(() => {
    let alive = true;
    getMe()
      .then((u) => {
        if (!alive) return;
        setMe(u);
        setLoaded(true);
      })
      .catch(() => alive && setLoaded(true));
    return () => {
      alive = false;
    };
  }, []);

  if (!loaded) return null;
  if (me) {
    return (
      <a className="lp-user" href="/app" title="open the dashboard">
        @{me.login}
      </a>
    );
  }
  return (
    <a className="lp-login" href="/auth/github/login">
      Log in with GitHub
    </a>
  );
}
