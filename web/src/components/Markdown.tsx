// The only place that renders HTML: Markdown is converted by marked and the
// result is sanitized by DOMPurify before it reaches the DOM.

import DOMPurify from "dompurify";
import { Marked } from "marked";
import { useMemo } from "react";

const marked = new Marked({ gfm: true, breaks: false, async: false });

const SAFE_URL = /^(?:https?:|mailto:|#|\/(?!\/))/i;
let hooked = false;

function installHooks(): void {
  if (hooked) return;
  hooked = true;
  DOMPurify.addHook("afterSanitizeAttributes", (node) => {
    for (const attr of ["href", "src"]) {
      const value = node.getAttribute(attr);
      if (value !== null && !SAFE_URL.test(value.trim())) node.removeAttribute(attr);
    }
    if (node.tagName === "A" && node.getAttribute("href")) {
      node.setAttribute("target", "_blank");
      node.setAttribute("rel", "noopener noreferrer nofollow");
    }
  });
}

/** Markdown → sanitized HTML string. Exported for tests. */
export function renderMarkdown(source: string): string {
  installHooks();
  const html = marked.parse(source) as string;
  return DOMPurify.sanitize(html, {
    USE_PROFILES: { html: true },
    FORBID_TAGS: ["style", "form", "input", "button", "textarea", "select", "iframe", "object", "embed", "svg", "math"],
    FORBID_ATTR: ["style", "class", "id"],
  });
}

export function Markdown({ source, className }: { source: string; className?: string }) {
  const html = useMemo(() => renderMarkdown(source), [source]);
  return <div className={["markdown", className].filter(Boolean).join(" ")} dangerouslySetInnerHTML={{ __html: html }} />;
}
