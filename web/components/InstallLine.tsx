"use client";

/* The dev-first conversion line: a board-voice install command that copies to the
   clipboard on click. The whole line is the hit target; the trailing label flips
   to `copied` and back. */

import { useState } from "react";

const INSTALL_CMD = "docker run --env-file .env ghcr.io/lumaprism/mergequeue";

export function InstallLine() {
  const [copied, setCopied] = useState(false);

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(INSTALL_CMD);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1600);
    } catch {
      setCopied(false);
    }
  };

  return (
    <button type="button" className="lp-install-cmd" onClick={copy} aria-label="Copy the install command">
      <span className="lp-install-prompt">$</span>
      <code className="lp-install-text">
        docker run … <b>mergequeue</b>
      </code>
      <span className="lp-install-copy" aria-hidden>
        {copied ? "copied" : "copy"}
      </span>
    </button>
  );
}
