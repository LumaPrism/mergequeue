import "./docs.css";
import { source } from "@/lib/source";
import { DocsLayout } from "fumadocs-ui/layouts/docs";
import { RootProvider } from "fumadocs-ui/provider/next";
import { baseOptions } from "@/lib/layout.shared";
import type { ReactNode } from "react";

export default function Layout({ children }: { children: ReactNode }) {
  return (
    <RootProvider
      // The whole app is dark (the `dark` class is hardcoded on <html>), so disable
      // next-themes entirely — no theme toggle, no injected theme <script> (which
      // React 19 warns about), no FOUC.
      theme={{ enabled: false }}
    >
      <DocsLayout tree={source.getPageTree()} {...baseOptions()}>
        {children}
      </DocsLayout>
    </RootProvider>
  );
}
