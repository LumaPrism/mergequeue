/// True when building the static GitHub Pages site (no backend) — set only by the
/// Pages workflow via NEXT_PUBLIC_STATIC_LANDING=1. The single switch for every
/// landing/docs change that hides the dashboard + sign-in and points CTAs at docs.
export const STATIC_SITE = process.env.NEXT_PUBLIC_STATIC_LANDING === "1";

/// Prefix for raw `<img src>` assets in public/. The Pages site is a project page
/// served under /mergequeue (next.config basePath), but a raw `<img>` doesn't get
/// that prefix automatically the way next/link does — so prepend it here. Empty
/// when self-hosted (no basePath).
export const ASSET_BASE = STATIC_SITE ? "/mergequeue" : "";
