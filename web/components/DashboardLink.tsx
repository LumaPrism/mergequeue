"use client";

/* Landing-nav Dashboard link (nav + footer), shown only once the GitHub App is
   registered. Before setup there's no usable dashboard — login dead-ends without
   an OAuth client — so the link would dangle; we hide it until the App exists.
   Gated behind !STATIC_SITE at the call site, so it never mounts in the export. */

import Link from "next/link";

import { useLandingAuth } from "@/lib/use-landing-auth";

export function DashboardLink() {
  const { loaded, registered } = useLandingAuth();

  if (!loaded || !registered) return null;
  return <Link href="/app">Dashboard</Link>;
}
