// The only place that renders HTML: Markdown is converted by marked and the
// result is sanitized by DOMPurify before it reaches the DOM.

import DOMPurify from "dompurify";
import { Marked } from "marked";
import { useMemo } from "react";
import { useT, type Translate } from "../i18n";
import { useMarkdown } from "../query/hooks";

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
export function renderMarkdown(source: string, t?: Translate): string {
  installHooks();
  const html = marked.parse(source) as string;
  const clean = DOMPurify.sanitize(html, {
    USE_PROFILES: { html: true },
    FORBID_TAGS: ["style", "form", "input", "button", "textarea", "select", "iframe", "object", "embed", "svg", "math"],
    FORBID_ATTR: ["style", "class", "id"],
  });
  if (!t) return clean;
  const element = document.createElement("div");
  element.innerHTML = clean;
  for (const a of element.querySelectorAll<HTMLAnchorElement>("a[data-branvil-kind][data-branvil-state]")) {
    const kind = a.dataset.branvilKind;
    const state = a.dataset.branvilState;
    if ((kind !== "ticket" && kind !== "pull") || (state !== "open" && state !== "closed" && state !== "merged")) continue;
    const description = `${t(`markdown.${kind}`)}, ${t(`pull.state.${state}`)}`;
    a.title = `${description}: ${a.title}`;
    a.setAttribute("aria-label", `${a.textContent}, ${a.title}`);
  }
  return element.innerHTML;
}

export function Markdown({ repo, source, className }: { repo: string; source: string; className?: string }) {
  const t = useT();
  const resolved = useMarkdown(repo, source);
  // Show the source while resolving (and if the request fails); no content
  // is hidden by a failed enhancement, and source changes cannot use old links.
  const markdown = resolved.data?.markdown ?? source;
  const html = useMemo(() => renderMarkdown(markdown, t), [markdown, t]);
  return <div className={["markdown", className].filter(Boolean).join(" ")} dangerouslySetInnerHTML={{ __html: html }} />;
}
