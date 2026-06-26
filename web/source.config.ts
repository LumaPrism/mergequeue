import { defineDocs, defineConfig } from "fumadocs-mdx/config";

export const docs = defineDocs({
  dir: "content/docs",
});

/**
 * SIGNAL BOX — the brand code theme. Amber keywords on near-black, with the
 * signal aspects threaded through syntax: green strings (clear), blue functions
 * (track-circuit), amber constants/types (the lamp). Palette mirrors the tokens
 * in globals.css so code blocks read in the same color language as the train.
 */
const signalBox = {
  name: "signal-box",
  type: "dark" as const,
  colors: {
    "editor.background": "#0a0b0f",
    "editor.foreground": "#e7e5e0",
  },
  tokenColors: [
    {
      scope: ["comment", "punctuation.definition.comment", "string.comment"],
      settings: { foreground: "#8c929d", fontStyle: "italic" },
    },
    {
      scope: [
        "keyword",
        "keyword.control",
        "storage",
        "storage.type",
        "storage.modifier",
        "keyword.operator.new",
        "keyword.operator.expression",
      ],
      settings: { foreground: "#e6a85c" },
    },
    {
      scope: ["keyword.operator", "punctuation", "meta.brace"],
      settings: { foreground: "#aab0ba" },
    },
    {
      scope: [
        "entity.name.function",
        "support.function",
        "meta.function-call.generic",
        "variable.function",
      ],
      settings: { foreground: "#7ba1de" },
    },
    {
      scope: ["string", "string.quoted", "string.template", "punctuation.definition.string"],
      settings: { foreground: "#5fc295" },
    },
    {
      scope: [
        "constant.numeric",
        "constant.language",
        "constant.character",
        "constant.other.symbol",
        "support.constant",
      ],
      settings: { foreground: "#ecbd7e" },
    },
    {
      scope: [
        "entity.name.type",
        "entity.name.class",
        "support.type",
        "support.class",
        "entity.other.inherited-class",
      ],
      settings: { foreground: "#e0b06a" },
    },
    {
      scope: ["entity.name.tag", "punctuation.definition.tag"],
      settings: { foreground: "#e58a92" },
    },
    {
      scope: ["entity.other.attribute-name"],
      settings: { foreground: "#e6a85c" },
    },
    {
      scope: [
        "variable",
        "variable.other",
        "meta.definition.variable",
        "support.variable",
      ],
      settings: { foreground: "#e7e5e0" },
    },
    {
      scope: ["variable.parameter", "meta.parameter"],
      settings: { foreground: "#aab0ba" },
    },
    {
      scope: ["meta.property-name", "support.type.property-name", "meta.object-literal.key"],
      settings: { foreground: "#7ba1de" },
    },
    {
      scope: ["markup.inserted", "punctuation.definition.inserted"],
      settings: { foreground: "#5fc295" },
    },
    {
      scope: ["markup.deleted", "punctuation.definition.deleted"],
      settings: { foreground: "#e58a92" },
    },
    {
      scope: ["invalid", "invalid.illegal"],
      settings: { foreground: "#e58a92" },
    },
  ],
};

export default defineConfig({
  mdxOptions: {
    rehypeCodeOptions: {
      themes: {
        light: signalBox,
        dark: signalBox,
      },
    },
  },
});
