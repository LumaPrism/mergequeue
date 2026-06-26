import { readFileSync } from "node:fs";
import { join } from "node:path";

import { ImageResponse } from "next/og";

export const runtime = "nodejs";
// Generate at build time so the page works under static export (GitHub Pages).
export const dynamic = "force-static";
export const alt = "mergequeue — CI-agnostic merge queue";
export const size = { width: 1200, height: 630 };
export const contentType = "image/png";

async function loadFont(weight: number): Promise<ArrayBuffer> {
  const url = `https://cdn.jsdelivr.net/npm/@fontsource/hanken-grotesk@5/files/hanken-grotesk-latin-${weight}-normal.woff`;
  const res = await fetch(url);
  if (!res.ok) throw new Error(`font ${weight}`);
  return res.arrayBuffer();
}

/// Bold display type makes it pop; returns undefined (satori default) if the CDN is unreachable.
async function loadFonts() {
  const weights = [400, 800] as const;
  try {
    const files = await Promise.all(weights.map(loadFont));
    return weights.map((weight, i) => ({
      name: "Hanken Grotesk",
      data: files[i],
      weight,
      style: "normal" as const,
    }));
  } catch {
    return undefined;
  }
}

export default async function OpengraphImage() {
  const logo = readFileSync(join(process.cwd(), "public", "logo.png"));
  const logoSrc = `data:image/png;base64,${logo.toString("base64")}`;
  const dots = ["#6b7280", "#5b82c4", "#4fa37a"]; // queued · testing · merge

  const fonts = await loadFonts();

  return new ImageResponse(
    (
      <div
        style={{
          width: "100%",
          height: "100%",
          display: "flex",
          flexDirection: "column",
          justifyContent: "space-between",
          padding: "76px",
          color: "#e6e4df",
          fontFamily: "Hanken Grotesk",
          background: "#0a0b0f",
          backgroundImage:
            "radial-gradient(900px 520px at 86% -12%, rgba(217,154,78,0.22), transparent 60%), radial-gradient(720px 480px at -4% 112%, rgba(91,130,196,0.16), transparent 62%)",
        }}
      >
        <div style={{ display: "flex", alignItems: "center", gap: "30px" }}>
          {/* eslint-disable-next-line @next/next/no-img-element */}
          <img src={logoSrc} width={132} height={132} style={{ borderRadius: 28 }} alt="" />
          <div style={{ display: "flex", flexDirection: "column" }}>
            <div style={{ display: "flex", fontSize: 86, fontWeight: 800, letterSpacing: -3 }}>
              <span>merge</span>
              <span style={{ color: "#d99a4e" }}>queue</span>
            </div>
            <div style={{ fontSize: 27, color: "#9498a1", marginTop: 4 }}>
              ci-agnostic merge queue
            </div>
          </div>
        </div>

        <div style={{ display: "flex", flexDirection: "column", gap: "22px" }}>
          <div style={{ display: "flex", alignItems: "center", gap: "20px" }}>
            <div style={{ display: "flex", gap: "12px" }}>
              {dots.map((c) => (
                <div key={c} style={{ width: 20, height: 20, borderRadius: 10, background: c }} />
              ))}
            </div>
            <div style={{ fontSize: 34, fontWeight: 800 }}>batch · test · land — or eject</div>
          </div>
          <div style={{ fontSize: 25, color: "#767c87" }}>
            self-hosted · works with any CI · on any plan
          </div>
        </div>
      </div>
    ),
    { ...size, fonts },
  );
}
