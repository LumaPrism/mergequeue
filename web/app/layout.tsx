import type { Metadata } from "next";
import { Hanken_Grotesk, DM_Mono, Space_Mono } from "next/font/google";
import "./globals.css";

// Fonts are exposed as CSS variables (--font-ui / --font-mono / --font-board) and
// consumed only through tokens in globals.css — swap the families here to restyle
// everything. UI = workhorse + display, mono = dense chrome, board = the signal voice.
const ui = Hanken_Grotesk({
  subsets: ["latin"],
  weight: ["400", "500", "600", "700", "800"],
  variable: "--font-ui",
});

const mono = DM_Mono({
  subsets: ["latin"],
  weight: ["400", "500"],
  variable: "--font-mono",
});

const board = Space_Mono({
  subsets: ["latin"],
  weight: ["400", "700"],
  variable: "--font-board",
});

export const metadata: Metadata = {
  metadataBase: new URL(process.env.NEXT_PUBLIC_SITE_URL ?? "http://localhost:3001"),
  title: "mergequeue — dashboard",
  description: "CI-agnostic merge queue — batch, test, land, or eject. Self-hosted, works with any CI.",
  openGraph: {
    title: "mergequeue",
    description: "CI-agnostic merge queue — batch, test, land, or eject. Self-hosted, works with any CI.",
    type: "website",
    siteName: "mergequeue",
  },
  twitter: {
    card: "summary_large_image",
    title: "mergequeue",
    description: "CI-agnostic merge queue — self-hosted, works with any CI.",
  },
};

export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html
      lang="en"
      className={`dark ${ui.variable} ${mono.variable} ${board.variable}`}
      suppressHydrationWarning
    >
      <body>{children}</body>
    </html>
  );
}
