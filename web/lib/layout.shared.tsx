import type { BaseLayoutProps } from "fumadocs-ui/layouts/shared";

import { ASSET_BASE, STATIC_SITE } from "@/lib/static-site";

export function baseOptions(): BaseLayoutProps {
  const home = {
    type: "custom" as const,
    children: (
      <a
        href={`${ASSET_BASE}/`}
        className="text-sm text-fd-muted-foreground transition-colors hover:text-fd-foreground"
      >
        Home
      </a>
    ),
  };
  return {
    nav: {
      url: "/docs",
      title: (
        <span className="flex items-center gap-2">
          <img src={`${ASSET_BASE}/logo.png`} alt="" className="size-5" />
          <span className="font-semibold">
            merge<span style={{ color: "var(--primary)" }}>queue</span>
          </span>
        </span>
      ),
    },
    githubUrl: "https://github.com/LumaPrism/mergequeue",
    links: STATIC_SITE ? [home] : [home, { text: "Dashboard", url: "/app" }],
  };
}
