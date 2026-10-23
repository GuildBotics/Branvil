import { act, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { fakeApi, json, renderApp, SESSION } from "../test/fakeApi";
import { choose } from "../test/mantine";
import { applyChange, LOST_AFTER_MS } from "../query/events";
import { pull } from "./fixtures";

const page = (items: unknown[]) => json(200, { items, next_cursor: null });
const repo = (path: string) => ({ path, default_branch: "main", clone_url: "", url: "", protected_branches: [], required_checks: [], labels: [], upstream: null, revision: 1 });

const SHELL = {
  "GET /api/v1/session": json(200, SESSION),
  "GET /api/v1/workspaces": page([
    { name: "acme", lanes: ["new"] },
    { name: "beta", lanes: ["new"] },
  ]),
  "GET /api/v1/admin/diagnostics": json(200, {}),
  "GET /api/v1/members": page([]),
};

/** The page's change stream, opened and dropped by the test. */
class FakeEventSource {
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSED = 2;
  static last: FakeEventSource | null = null;
  readyState = FakeEventSource.CONNECTING;
  onopen: (() => void) | null = null;
  onerror: (() => void) | null = null;
  constructor() {
    FakeEventSource.last = this;
  }
  addEventListener() {}
  close() {
    this.readyState = FakeEventSource.CLOSED;
  }
  open() {
    this.readyState = FakeEventSource.OPEN;
    act(() => this.onopen?.());
  }
  /** Dropped, the browser retrying it as EventSource does. */
  drop() {
    this.readyState = FakeEventSource.CONNECTING;
    act(() => this.onerror?.());
  }
}

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe("pages", () => {
  it("opens the first workspace's board, then the one shown last", async () => {
    fakeApi({
      ...SHELL,
      "GET /api/v1/workspaces/acme": json(200, { name: "acme", lanes: ["new"] }),
      "GET /api/v1/workspaces/acme/board/cards": page([]),
      "GET /api/v1/workspaces/beta": json(200, { name: "beta", lanes: ["new"] }),
      "GET /api/v1/workspaces/beta/board/cards": page([]),
      "GET /api/v1/repos": page([]),
    });
    const first = renderApp("/");
    await waitFor(() => expect(window.location.pathname).toBe("/acme"));
    first.unmount();
    renderApp("/beta");
    expect(await screen.findByText("beta · 0 open")).toBeInTheDocument();
    renderApp("/");
    await waitFor(() => expect(window.location.pathname).toBe("/beta"));
  });

  it("offers to create a workspace when there is none", async () => {
    fakeApi({ ...SHELL, "GET /api/v1/workspaces": page([]) });
    renderApp("/");
    expect(await screen.findByRole("heading", { name: "No workspace yet" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "New workspace" })).toBeInTheDocument();
  });

  it("lists the pull requests of the workspace's repositories, newest first, and narrows them to one repository", async () => {
    const older = pull({ updated_at: "2026-10-13T10:00:00.000Z" });
    const newer = pull({
      url: `${window.location.origin}/acme/web/-/pulls/1`,
      repository: "acme/web",
      number: 1,
      label: "!1",
      title: "Draft work",
      draft: true,
      updated_at: "2026-10-15T10:00:00.000Z",
    });
    const fake = fakeApi({
      ...SHELL,
      "GET /api/v1/repos": page([repo("acme/demo"), repo("acme/web"), repo("beta/x")]),
      "GET /api/v1/repos/acme/demo/pulls": page([older]),
      "GET /api/v1/repos/acme/web/pulls": page([newer]),
      "GET /api/v1/repos/acme/demo/pulls/3/checks": json(200, { head_sha: "a", base_sha: "b", behind_by: 0, checks: [], checks_expected: false, base_checks: 0 }),
      "GET /api/v1/repos/acme/web/pulls/1/checks": json(200, { head_sha: "a", base_sha: "b", behind_by: 0, checks: [], checks_expected: false, base_checks: 0 }),
    });
    const user = userEvent.setup();
    renderApp("/acme/-/pulls");
    const table = await screen.findByRole("table", { name: "Pull requests" });
    await waitFor(() => expect(within(table).getAllByRole("row")).toHaveLength(3));
    const rows = within(table).getAllByRole("row").slice(1);
    expect(rows.map((r) => r.getAttribute("aria-label"))).toEqual(["acme/web !1 Draft work", "acme/demo !3 Fix login"]);
    expect(rows[1]).toHaveAttribute("href", "/acme/demo/-/pulls/3");
    expect(within(rows[0] as HTMLElement).getByText("Draft")).toBeInTheDocument();
    expect(within(table).getByRole("columnheader", { name: "Reviewers" })).toBeInTheDocument();
    for (const row of rows) {
      expect(within(row).getByRole("img", { name: "Reviewers: owner (Requested)" })).toBeInTheDocument();
      expect(within(row).queryByRole("button")).not.toBeInTheDocument();
    }
    // Another workspace's repository is not read.
    expect(fake.calls("GET", "/api/v1/repos/beta/x/pulls")).toHaveLength(0);

    // A repository's page is this list for that repository.
    await choose(user, "Repository", "acme/demo");
    await waitFor(() => expect(window.location.pathname).toBe("/acme/demo"));
    await waitFor(() => expect(within(screen.getByRole("table", { name: "Pull requests" })).getAllByRole("row")).toHaveLength(2));

    await choose(user, "State", "State: Closed");
    await waitFor(() => expect(fake.calls("GET", "/api/v1/repos/acme/demo/pulls").some((c) => c.search.includes("state=closed"))).toBe(true));
  });

  it("says when no pull request is open", async () => {
    fakeApi({ ...SHELL, "GET /api/v1/repos": page([repo("acme/demo")]), "GET /api/v1/repos/acme/demo/pulls": page([]) });
    renderApp("/acme/-/pulls");
    expect(await screen.findByText("No open pull requests.")).toBeInTheDocument();
  });

  it("shows a commit's message, author, and parents", async () => {
    const sha = "d".repeat(40);
    const parent = "e".repeat(40);
    fakeApi({
      ...SHELL,
      [`GET /api/v1/repos/acme/demo/commits/${sha}`]: json(200, {
        sha,
        message: "Fix login\n\nHandle an empty token.",
        author: "Aiko <aiko@example.com>",
        authored_at: "2026-10-14T10:00:00.000Z",
        parents: [parent],
      }),
      [`GET /api/v1/repos/acme/demo/commits/${sha}/checks`]: json(200, { sha, checks: [], checks_expected: false }),
    });
    renderApp(`/acme/demo/-/commits/${sha}`);
    expect(await screen.findByRole("heading", { name: "Fix login" })).toBeInTheDocument();
    expect(screen.getByText("Handle an empty token.")).toBeInTheDocument();
    expect(screen.getByText("Aiko <aiko@example.com>")).toBeInTheDocument();
    expect(screen.getByTitle(parent).closest("a")).toHaveAttribute("href", `/acme/demo/-/commits/${parent}`);
    const checks = await screen.findByRole("region", { name: "Checks" });
    expect(await within(checks).findByText("This repository runs no checks for this commit.")).toBeInTheDocument();
  });

  it("shows a commit's checks on its page and opens a job from them", async () => {
    const sha = "d".repeat(40);
    const fake = fakeApi({
      ...SHELL,
      [`GET /api/v1/repos/acme/demo/commits/${sha}`]: json(200, { sha, message: "Fix login", author: "Aiko", authored_at: "2026-10-14T10:00:00.000Z", parents: [] }),
      [`GET /api/v1/repos/acme/demo/commits/${sha}/checks`]: json(200, {
        sha,
        checks: [
          { name: "test", status: "completed", conclusion: "failure", attempt_id: "att_1", attempt: 1, artifact_names: [] },
          { name: "lint", status: "queued", conclusion: null },
        ],
        checks_expected: true,
      }),
      "GET /api/v1/repos/acme/demo/jobs/att_1": json(200, { id: "att_1", job: "test", attempt: 1, sha, status: "completed", conclusion: "failure", details: "exit code 101", artifact_names: ["report"] }),
      "GET /api/v1/repos/acme/demo/jobs/att_1/log": json(200, { log: "test login ... FAILED", log_bytes: 20, truncated: false }),
    });
    const user = userEvent.setup();
    renderApp(`/acme/demo/-/commits/${sha}`);
    const checks = await screen.findByRole("region", { name: "Checks" });
    expect(await within(checks).findByText("failing")).toBeInTheDocument();
    expect(within(checks).getByText("queued")).toBeInTheDocument();
    expect(await within(checks).findByText("Reason: exit code 101")).toBeInTheDocument();
    // The commit's own page does not link to itself.
    expect(within(checks).getByTitle(sha).closest("a")).toBeNull();

    await user.click(within(checks).getByRole("button", { name: "Open check test" }));
    const job = await screen.findByRole("dialog", { name: /test/ });
    expect(await within(job).findByText("test login ... FAILED")).toBeInTheDocument();
    expect(within(job).getByRole("link", { name: "report" })).toHaveAttribute("href", `/api/v1/repos/acme%2Fdemo/commits/${sha}/artifacts/report`);
    expect(fake.calls("GET", `/api/v1/repos/acme/demo/commits/${sha}/checks`)).toHaveLength(1);
  });

  it("shows the checks of the default branch's tip on a repository's page", async () => {
    const tip = "f".repeat(40);
    const fake = fakeApi({
      ...SHELL,
      "GET /api/v1/repos": page([repo("acme/demo")]),
      "GET /api/v1/repos/acme/demo": json(200, { ...repo("acme/demo"), default_branch: "trunk" }),
      "GET /api/v1/repos/acme/demo/branches/trunk": json(200, { name: "trunk", sha: tip }),
      [`GET /api/v1/repos/acme/demo/commits/${tip}/checks`]: json(200, {
        sha: tip,
        checks: [{ name: "test", status: "in_progress", conclusion: null, attempt_id: "att_2", attempt: 1 }],
        checks_expected: true,
      }),
      "GET /api/v1/repos/acme/demo/pulls": page([]),
    });
    renderApp("/acme/demo");
    const checks = await screen.findByRole("region", { name: "Checks of trunk" });
    expect(await within(checks).findByText("pending")).toBeInTheDocument();
    expect(within(checks).getByText("in progress")).toBeInTheDocument();
    expect(within(checks).getByTitle(tip).closest("a")).toHaveAttribute("href", `/acme/demo/-/commits/${tip}`);
    expect(within(checks).getByRole("button", { name: "Open check test" })).toBeInTheDocument();
    expect(await screen.findByText("No open pull requests.")).toBeInTheDocument();
    expect(fake.calls("GET", "/api/v1/repos/acme/demo/branches/trunk")).toHaveLength(1);
  });

  it("says so on a repository's page when its default branch has no commit yet", async () => {
    fakeApi({
      ...SHELL,
      "GET /api/v1/repos": page([repo("acme/demo")]),
      "GET /api/v1/repos/acme/demo": json(200, repo("acme/demo")),
      "GET /api/v1/repos/acme/demo/pulls": page([]),
    });
    renderApp("/acme/demo");
    expect(await screen.findByText("main has no commits yet.")).toBeInTheDocument();
  });

  it("keeps the default branch checks below the list in the same card, with a collapsed workspace summary", async () => {
    const tip = "f".repeat(40);
    const fake = fakeApi({
      ...SHELL,
      "GET /api/v1/repos": page([repo("acme/demo")]),
      "GET /api/v1/repos/acme/demo": json(200, repo("acme/demo")),
      "GET /api/v1/repos/acme/demo/branches/main": json(200, { name: "main", sha: tip }),
      [`GET /api/v1/repos/acme/demo/commits/${tip}/checks`]: json(200, { sha: tip, checks: [], checks_expected: false }),
      "GET /api/v1/repos/acme/demo/pulls": page([]),
    });
    renderApp("/acme/-/pulls");
    const list = await screen.findByRole("table", { name: "Pull requests" });
    await waitFor(() => expect(fake.calls("GET", `/api/v1/repos/acme/demo/commits/${tip}/checks`)).toHaveLength(1));
    const summary = screen.getByRole("button", { expanded: false });
    expect(list.compareDocumentPosition(summary) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(list.closest(".mantine-Card-root")).toBe(summary.closest(".mantine-Card-root"));
    const user = userEvent.setup();
    await user.click(summary);
    const sha = await screen.findByTitle(tip);
    expect(sha.closest("a")).toHaveAttribute("href", `/acme/demo/-/commits/${tip}`);
  });

  it("refreshes review status in the list after a pull event, preserving row navigation", async () => {
    const pr = pull({ reviewers: [], review_states: [] });
    const fake = fakeApi({
      ...SHELL,
      "GET /api/v1/repos": page([repo("acme/demo")]),
      "GET /api/v1/repos/acme/demo/pulls": [page([pr]), page([{ ...pr, review_states: [
        { member: "review-bot", requested: false, verdict: "approve", outdated: false },
      ] }])],
    });
    const { client } = renderApp("/acme/-/pulls");
    const row = await screen.findByRole("row", { name: "acme/demo !3 Fix login" });
    expect(await within(row).findByRole("img", { name: "Reviewers: No one" })).toBeInTheDocument();
    act(() => applyChange({ kind: "pull", path: "acme/demo", id: "3" }, client));
    const status = await within(row).findByRole("img", { name: "Reviewers: review-bot (Approved)" });
    expect(fake.calls("GET", "/api/v1/repos/acme/demo/pulls")).toHaveLength(2);
    expect(fake.calls("GET", "/api/v1/repos/acme/demo/pulls/3/activity")).toHaveLength(0);
    await userEvent.setup().click(status);
    expect(window.location.pathname).toBe("/acme/demo/-/pulls/3");
  });

  it("warns only when CI cannot run or an upstream cannot be followed, and a dismissed warning stays away", async () => {
    fakeApi({
      ...SHELL,
      "GET /api/v1/repos": page([]),
      "GET /api/v1/admin/diagnostics": json(200, {
        runner: { state: "no_runner", detail: "No runner has contacted Branvil.", mode: "auto" },
        upstream: { state: "problems", problems: [{ repository: "acme/demo", problem: "The reader's token was refused." }] },
      }),
    });
    const user = userEvent.setup();
    renderApp("/-/settings/members");
    const warnings = await screen.findByRole("region", { name: "Warnings" });
    expect(await within(warnings).findByText(/No CI runner is connected/)).toBeInTheDocument();
    expect(within(warnings).getByText("The upstream of acme/demo cannot be followed: The reader's token was refused.")).toBeInTheDocument();
    expect(within(warnings).getByRole("link", { name: "Open the repository settings" })).toHaveAttribute("href", "/-/settings");
    await user.click(within(warnings).getAllByRole("button", { name: "Dismiss" })[0] as HTMLElement);
    expect(within(warnings).queryByText(/No CI runner is connected/)).not.toBeInTheDocument();
    expect(within(warnings).getByText(/cannot be followed/)).toBeInTheDocument();
  });

  it("does not warn about the runner when CI is turned off", async () => {
    fakeApi({
      ...SHELL,
      "GET /api/v1/members": page([]),
      "GET /api/v1/admin/diagnostics": json(200, { runner: { state: "no_runner", detail: "", mode: "off" }, upstream: { state: "following", problems: [] } }),
    });
    renderApp("/-/settings/members");
    await screen.findByRole("list", { name: "Members" });
    expect(screen.queryByRole("region", { name: "Warnings" })).not.toBeInTheDocument();
  });
  it("says nothing while the page is connected, and warns once the connection has been lost for a while", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.stubGlobal("EventSource", FakeEventSource);
    fakeApi({ ...SHELL, "GET /api/v1/admin/diagnostics": json(200, { upstream: { state: "following", problems: [] } }) });
    renderApp("/-/settings/members");
    await screen.findByRole("list", { name: "Members" });
    const stream = FakeEventSource.last as FakeEventSource;
    stream.open();
    expect(screen.queryByRole("status")).not.toBeInTheDocument();

    // A moment's drop, back before the warning is due, is not worth one.
    stream.drop();
    act(() => vi.advanceTimersByTime(LOST_AFTER_MS - 1));
    stream.open();
    act(() => vi.advanceTimersByTime(LOST_AFTER_MS));
    expect(screen.queryByRole("region", { name: "Warnings" })).not.toBeInTheDocument();

    // A longer one is, counted from its own start, and cannot be dismissed.
    stream.drop();
    act(() => vi.advanceTimersByTime(LOST_AFTER_MS - 1));
    expect(screen.queryByRole("region", { name: "Warnings" })).not.toBeInTheDocument();
    act(() => vi.advanceTimersByTime(1));
    const warnings = screen.getByRole("region", { name: "Warnings" });
    expect(within(warnings).getByText("The connection to the server is lost, so what you see may be out of date. Reconnecting…")).toBeInTheDocument();
    expect(within(warnings).queryByRole("button", { name: "Dismiss" })).not.toBeInTheDocument();

    stream.open();
    expect(screen.queryByRole("region", { name: "Warnings" })).not.toBeInTheDocument();
  });
});
