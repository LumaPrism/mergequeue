import { createMDX } from "fumadocs-mdx/next";

// PAGES_EXPORT builds a static, landing-only site for GitHub Pages (served under
// /mergequeue). The CI workflow strips the dashboard/docs/api routes first, so the
// export contains just the marketing landing — rewrites (a server feature) are off.
const isExport = process.env.PAGES_EXPORT === "1";

/** @type {import('next').NextConfig} */
const nextConfig = isExport
  ? {
      output: "export",
      basePath: "/mergequeue",
      images: { unoptimized: true },
    }
  : {
      async rewrites() {
        // Proxy API + auth to the backend in dev so the dashboard is same-origin —
        // and, crucially, so the auth callback's Set-Cookie lands on this origin
        // (the session cookie must be readable by /api/me from the dashboard).
        const backend = process.env.MQ_BACKEND_URL ?? "http://localhost:8080";
        return [
          { source: "/api/:path*", destination: `${backend}/api/:path*` },
          { source: "/auth/:path*", destination: `${backend}/auth/:path*` },
        ];
      },
    };

const withMDX = createMDX();

export default withMDX(nextConfig);
