import type { BaseLayoutProps } from "fumadocs-ui/layouts/shared";

import { ASSET_BASE, STATIC_SITE } from "@/lib/static-site";

export function baseOptions(): BaseLayoutProps {
  return {
    nav: {
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
    // The static Pages site has no backend — drop the dashboard link there.
    links: STATIC_SITE
      ? [{ text: "Home", url: "/" }]
      : [
          { text: "Home", url: "/" },
          { text: "Dashboard", url: "/app" },
        ],
  };
}
