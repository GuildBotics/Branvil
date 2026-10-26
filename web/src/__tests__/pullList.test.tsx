import { act, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { fakeApi, json, renderApp, SESSION } from "../test/fakeApi";
import { choose } from "../test/mantine";
import { applyChange } from "../query/events";
import { pull } from "./fixtures";

const page = (items: unknown[], next_cursor: string | null = null) => json(200, { items, next_cursor });
const repository = (path: string) => ({ path, default_branch: "main", upstream: null });
const shell = {
  "GET /api/v1/session": json(200, SESSION),
  "GET /api/v1/workspaces": page([{ name: "acme", lanes: ["new"] }]),
  "GET /api/v1/admin/diagnostics": json(200, {}),
  "GET /api/v1/members": page([]),
};
const row = (repo: string, number: number, second: number) => pull({
  repository: repo, number, label: `!${number}`, title: `Change ${number}`,
  url: `${window.location.origin}/${repo}/-/pulls/${number}`,
  updated_at: new Date(Date.UTC(2026, 9, 20, 0, 0, second)).toISOString(),
  state: "closed", merged: number % 2 === 0,
});
const rows = () => within(screen.getByRole("table", { name: "Pull requests" })).getAllByRole("row").slice(1);

describe("pull request pagination", () => {
  it.each([false, true])("loads older PRs in global order (workspace: %s), resets state and refetches on events", async (workspace) => {
    const demo = Array.from({ length: 61 }, (_, i) => row("acme/demo", 61 - i, 61 - i));
    const other = row("acme/web", 1, 31.5);
    // Keep millisecond precision for the row between demo !31 and !32.
    other.updated_at = "2026-10-20T00:00:31.500Z";
    const fake = fakeApi({
      ...shell,
      "GET /api/v1/repos": page([repository("acme/demo"), repository("acme/web")]),
      "GET /api/v1/repos/acme/demo/pulls": (req) => {
        const q = new URLSearchParams(req.search);
        if (q.get("state") === "open") return page([]);
        const start = Number(q.get("cursor") ?? 0);
        return page(demo.slice(start, start + 30), start + 30 < demo.length ? String(start + 30) : null);
      },
      "GET /api/v1/repos/acme/web/pulls": (req) => new URLSearchParams(req.search).get("state") === "open" ? page([]) : page([other]),
      "GET /api/v1/repos/acme/demo/landing": json(200, { items: [] }),
      "GET /api/v1/repos/acme/demo/branches/main": json(200, { name: "main", sha: "a" }),
      "GET /api/v1/repos/acme/web/branches/main": json(200, { name: "main", sha: "b" }),
      "GET /api/v1/repos/acme/demo/commits/a/checks": json(200, { checks: [] }),
      "GET /api/v1/repos/acme/web/commits/b/checks": json(200, { checks: [] }),
    });
    const user = userEvent.setup();
    const { client } = renderApp(workspace ? "/acme/-/pulls" : "/acme/demo");
    await screen.findByText("No open pull requests.");
    await choose(user, "State", "State: Closed");
    await waitFor(() => expect(rows()).toHaveLength(30));
    expect(rows()[0]).toHaveAttribute("aria-label", "acme/demo !61 Change 61");
    expect(rows()[29]).toHaveAttribute("aria-label", "acme/demo !32 Change 32");
    await user.click(screen.getByRole("button", { name: "Load more pull requests" }));
    await waitFor(() => expect(rows()).toHaveLength(60));
    await user.click(screen.getByRole("button", { name: "Load more pull requests" }));
    await waitFor(() => expect(rows()).toHaveLength(workspace ? 62 : 61));
    const expected = workspace ? [...demo.slice(0, 30), other, ...demo.slice(30)] : demo;
    expect(rows().map((r) => r.getAttribute("aria-label"))).toEqual(expected.map((p) => `${p.repository} ${p.label} ${p.title}`));
    expect(screen.queryByRole("button", { name: "Load more pull requests" })).not.toBeInTheDocument();
    expect(fake.calls("GET", "/api/v1/repos/acme/demo/pulls").map((r) => new URLSearchParams(r.search).get("cursor"))).toEqual([null, null, "30", "60"]);
    if (!workspace) expect(fake.calls("GET", "/api/v1/repos/acme/web/pulls")).toHaveLength(0);

    // Refetch starts at the first page and rebuilds buffered tails, rather than
    // retaining rows from before the PR's update.
    demo[0] = { ...demo[0]!, title: "Changed after loading" };
    act(() => applyChange({ kind: "pull", path: "acme/demo", id: "61" }, client));
    await screen.findByText("Changed after loading");
    expect(rows()).toHaveLength(workspace ? 62 : 61);
    await choose(user, "State", "State: Any");
    await waitFor(() => expect(rows()).toHaveLength(30));
    const all = fake.calls("GET", "/api/v1/repos/acme/demo/pulls").filter((r) => new URLSearchParams(r.search).get("state") === "all");
    expect(all).toHaveLength(1);
    expect(new URLSearchParams(all[0]!.search).get("cursor")).toBeNull();
  }, 30_000);

  it("keeps the loaded page and permits retry after an older page fails", async () => {
    let fail = true;
    const demo = Array.from({ length: 31 }, (_, i) => row("acme/demo", 31 - i, 31 - i));
    fakeApi({
      ...shell,
      "GET /api/v1/repos": page([repository("acme/demo")]),
      "GET /api/v1/repos/acme/demo/pulls": (req) => {
        const q = new URLSearchParams(req.search);
        if (q.get("state") === "open") return page([]);
        if (!q.has("cursor")) return page(demo.slice(0, 30), "older");
        return fail ? json(503, { error: { code: "unavailable", message: "Try again" } }) : page(demo.slice(30));
      },
    });
    const user = userEvent.setup();
    renderApp("/acme/-/pulls");
    await screen.findByText("No open pull requests.");
    await choose(user, "State", "State: Closed");
    await waitFor(() => expect(rows()).toHaveLength(30));
    await user.click(screen.getByRole("button", { name: "Load more pull requests" }));
    await screen.findByText("Try again");
    expect(rows()).toHaveLength(30);
    fail = false;
    await user.click(screen.getByRole("button", { name: "Load more pull requests" }));
    await waitFor(() => expect(rows()).toHaveLength(31));
    expect(screen.queryByText("Try again")).not.toBeInTheDocument();
  });
});
