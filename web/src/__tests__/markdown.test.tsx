import { render } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { Markdown, renderMarkdown } from "../components/Markdown";

describe("Markdown rendering", () => {
  it("renders Markdown", () => {
    const html = renderMarkdown("# Title\n\n**bold** and `code`");
    expect(html).toContain("<h1>Title</h1>");
    expect(html).toContain("<strong>bold</strong>");
    expect(html).toContain("<code>code</code>");
  });

  it("removes scripts, event handlers, and dangerous URLs", () => {
    const html = renderMarkdown(
      [
        "<script>alert(1)</script>",
        '<img src="x" onerror="alert(2)">',
        "[click](javascript:alert(3))",
        '<a href="data:text/html,<script>alert(4)</script>">d</a>',
        '<iframe src="https://example.com"></iframe>',
        '<p style="position:fixed" onclick="alert(5)">p</p>',
        '<form action="/api/v1/session"><input name="x"></form>',
      ].join("\n\n"),
    );
    expect(html).not.toMatch(/<script/i);
    expect(html).not.toMatch(/onerror|onclick/i);
    expect(html).not.toMatch(/javascript:/i);
    expect(html).not.toMatch(/data:text/i);
    expect(html).not.toMatch(/<iframe|<form|<input|style=/i);
  });

  it("opens links in a new context without referrer or opener", () => {
    const { container } = render(<Markdown source="[docs](https://example.com/x)" />);
    const a = container.querySelector("a");
    expect(a?.getAttribute("href")).toBe("https://example.com/x");
    expect(a?.getAttribute("rel")).toBe("noopener noreferrer nofollow");
    expect(a?.getAttribute("target")).toBe("_blank");
  });
});
