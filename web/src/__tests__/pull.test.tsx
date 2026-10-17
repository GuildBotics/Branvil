import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { apiError, fakeApi, json, renderApp, SESSION } from "../test/fakeApi";
import { BASE, HEAD, NEW_HEAD, pull } from "./fixtures";

const P = "/api/v1/repos/acme/demo/pulls/3";

function pullRoutes(extra: Record<string, Parameters<typeof fakeApi>[0][string]> = {}) {
  return {
    "GET /api/v1/session": json(200, SESSION),
    "GET /api/v1/workspaces": json(200, { items: [{ name: "acme", lanes: [] }], next_cursor: null }),
    "GET /api/v1/admin/diagnostics": json(200, {}),
    "GET /api/v1/members": json(200, { items: [], next_cursor: null }),
    "GET /api/v1/repos/acme/demo": json(200, {
      path: "acme/demo",
      default_branch: "main",
      clone_url: "",
      url: "",
      protected_branches: ["main"],
      required_checks: ["test"],
      revision: 1,
    }),
    [`GET ${P}`]: json(200, pull()),
    [`GET ${P}/activity`]: json(200, {
      comments: [
        {
          id: "cmt_1",
          author: "aiko",
          body: "Workflow failed",
          meta: { routing: "suppress", kind: "workflow_error" },
          created_at: "2026-10-14T10:01:00.000Z",
          reactions: [],
          url: "",
        },
      ],
      reviews: [],
      threads: [],
    }),
    [`GET ${P}/threads`]: json(200, {
      items: [
        {
          id: "ic_1",
          resolved: false,
          outdated: false,
          path: "src/login.rs",
          line: 12,
          side: "RIGHT",
          commit: HEAD,
          comments: [{ id: "ic_1", author: "owner", body: "Why?", meta: null, created_at: "2026-10-14T10:02:00.000Z", reactions: [], url: "", reply_to_id: null }],
        },
      ],
      next_cursor: null,
    }),
    [`GET ${P}/files`]: json(200, {
      items: [{ path: "src/login.rs", previous_path: null, status: "modified", additions: 2, deletions: 1, patch: "@@ -1,2 +1,3 @@\n-old\n+new\n+more", patch_complete: true }],
      next_cursor: null,
    }),
    [`GET ${P}/checks`]: json(200, {
      head_sha: HEAD,
      base_sha: BASE,
      behind_by: 0,
      checks: [{ name: "test", status: "completed", conclusion: "failure", attempt_id: "att_1", attempt: 1 }],
      checks_expected: true,
      base_checks: 1,
    }),
    "GET /api/v1/repos/acme/demo/jobs/att_1": json(200, {
      id: "att_1",
      job: "test",
      attempt: 1,
      sha: HEAD,
      status: "completed",
      conclusion: "failure",
      started_at: null,
      finished_at: null,
      details: "exit code 101",
      artifact_names: [],
    }),
    "GET /api/v1/repos/acme/demo/jobs/att_1/log": json(200, { log: "test login ... FAILED", log_bytes: 90000, truncated: true }),
    ...extra,
  };
}

describe("pull request review", () => {
  it("shows the description (or 'Not provided'), the files in a drawer, checks with reason and a log in a drawer, and status notices", async () => {
    const fake = fakeApi(pullRoutes());
    const user = userEvent.setup();
    renderApp("/acme/demo/-/pulls/3");

    expect(await screen.findByRole("heading", { name: /Fix login/ })).toBeInTheDocument();
    const description = screen.getByRole("region", { name: "Description" });
    expect(within(description).getByText("Not provided")).toBeInTheDocument();
    expect(await screen.findByLabelText("Status notice")).toHaveTextContent("Workflow failed");
    expect(screen.getByText("Why?")).toBeInTheDocument();

    // The changed files open in a drawer from the facts line; one file opens at once.
    await user.click(await screen.findByRole("button", { name: /^1 file\(s\)/ }));
    const files = await screen.findByRole("dialog", { name: /Changed files/ });
    expect(within(files).getByTitle(HEAD)).toBeInTheDocument();
    expect(within(files).getByTitle(BASE)).toBeInTheDocument();
    expect(await within(files).findByText("+more")).toBeInTheDocument();
    await user.click(within(files).getByRole("button", { name: "Close" }));

    // A check row says why it failed and opens its job in a drawer.
    expect(await screen.findByText("Reason: exit code 101")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Open check test" }));
    const check = await screen.findByRole("dialog", { name: /test/ });
    expect(await within(check).findByText("test login ... FAILED")).toBeInTheDocument();
    expect(fake.calls("GET", "/api/v1/repos/acme/demo/jobs/att_1/log")[0]?.search).toBe("?tail_bytes=16384");
    expect(within(check).getByRole("button", { name: "Retry" })).toBeEnabled();
  });

  it("opens the changed files at a thread's file from the thread", async () => {
    fakeApi(pullRoutes());
    const user = userEvent.setup();
    renderApp("/acme/demo/-/pulls/3");
    await user.click(await screen.findByRole("button", { name: "src/login.rs:12" }));
    const files = await screen.findByRole("dialog", { name: /Changed files/ });
    expect(await within(files).findByText("+more")).toBeInTheDocument();
  });

  it("sends the displayed head with a review and handles head_moved by reloading", async () => {
    const fake = fakeApi(
      pullRoutes({
        [`GET ${P}`]: [json(200, pull()), json(200, pull({ head_sha: NEW_HEAD }))],
        [`POST ${P}/reviews`]: apiError(409, "head_moved", "The head moved."),
      }),
    );
    const user = userEvent.setup();
    renderApp("/acme/demo/-/pulls/3");
    await screen.findByRole("heading", { name: /Fix login/ });

    await user.click(screen.getByRole("button", { name: "Approve" }));

    expect(await screen.findByText(/The head moved after you loaded aaaaaaaaaa/)).toBeInTheDocument();
    const review = fake.calls("POST", `${P}/reviews`)[0];
    expect(review?.body).toEqual({ verdict: "approve", body: "", commit: HEAD });
    expect(review?.headers["X-Branvil-CSRF"]).toBe("csrf-123");
    // The pull request is reloaded and now shows the new head.
    await waitFor(() => expect(fake.calls("GET", P).length).toBeGreaterThanOrEqual(2));
    expect(await screen.findAllByTitle(NEW_HEAD)).not.toHaveLength(0);
  });

  it("shows each reviewer's state on their avatar, and requests and withdraws reviews from the menu", async () => {
    const fake = fakeApi(
      pullRoutes({
        "GET /api/v1/members": json(200, {
          items: [
            { name: "owner", display_name: "Owner", kind: "owner", accounts: [] },
            { name: "aiko", display_name: "Aiko", kind: "agent", accounts: [] },
            { name: "ken", display_name: "Ken", kind: "agent", accounts: [] },
            { name: "mia", display_name: "Mia", kind: "agent", accounts: [] },
            { name: "ci", display_name: "CI", kind: "runner", accounts: [] },
          ],
          next_cursor: null,
        }),
        [`GET ${P}/activity`]: json(200, {
          comments: [],
          reviews: [
            { id: "rv_1", author: "ken", verdict: "request_changes", body: "", commit: BASE, submitted_at: "2026-10-14T10:03:00.000Z", reactions: [] },
            { id: "rv_2", author: "ken", verdict: "approve", body: "", commit: HEAD, submitted_at: "2026-10-14T10:04:00.000Z", reactions: [] },
            { id: "rv_3", author: "mia", verdict: "comment", body: "Hm.", commit: BASE, submitted_at: "2026-10-14T10:05:00.000Z", reactions: [] },
          ],
          threads: [],
        }),
        [`PUT ${P}/reviewers/mia`]: json(200, {}),
        [`DELETE ${P}/reviewers/owner`]: json(200, {}),
      }),
    );
    const user = userEvent.setup();
    renderApp("/acme/demo/-/pulls/3");
    // An open request, the last verdict on the head, and a verdict on an older revision.
    const trigger = await screen.findByRole("button", {
      name: "Reviewers: ken (Approved), mia (Commented (older revision)), owner (Requested) — click to request or withdraw",
    });
    await user.click(trigger);
    // The author and runners are not offered.
    expect(await screen.findByRole("menuitem", { name: /^owner/ })).toHaveAttribute("aria-checked", "true");
    expect(screen.getByRole("menuitem", { name: /^ken/ })).toHaveAttribute("aria-checked", "false");
    expect(screen.queryByRole("menuitem", { name: /^aiko/ })).not.toBeInTheDocument();
    expect(screen.queryByRole("menuitem", { name: /^ci/ })).not.toBeInTheDocument();

    await user.click(screen.getByRole("menuitem", { name: /^mia/ }));
    await waitFor(() => expect(fake.calls("PUT", `${P}/reviewers/mia`)).toHaveLength(1));
    await user.click(screen.getByRole("menuitem", { name: /^owner/ }));
    await waitFor(() => expect(fake.calls("DELETE", `${P}/reviewers/owner`)).toHaveLength(1));
    expect(fake.calls("DELETE", `${P}/reviewers/owner`)[0]?.headers["X-Branvil-CSRF"]).toBe("csrf-123");
  });

  it("merges with the expected head and base and shows the refusal", async () => {
    const fake = fakeApi(
      pullRoutes({ [`POST ${P}/merge`]: apiError(409, "not_mergeable", "Required check test did not succeed.") }),
    );
    const user = userEvent.setup();
    renderApp("/acme/demo/-/pulls/3");
    expect(await screen.findByText("Required checks: test")).toBeInTheDocument();
    await user.click(await screen.findByRole("button", { name: "Merge (fast-forward)" }));

    expect(await screen.findByText("Not mergeable: Required check test did not succeed.")).toBeInTheDocument();
    expect(fake.calls("POST", `${P}/merge`)[0]?.body).toEqual({ expected_head: HEAD, expected_base: BASE });
  });

  const UPSTREAM_REPO = json(200, {
    path: "acme/demo",
    default_branch: "main",
    clone_url: "",
    url: "",
    protected_branches: ["main"],
    required_checks: ["test"],
    labels: [],
    upstream: {
      provider: "github",
      host: "GitHub",
      repository: "octo/demo",
      url: "https://github.com/octo/demo",
      reader: { member: "aiko", chosen: false },
      following: { tip: BASE, checked_at: null, followed_at: null, error: null, stopped: null },
    },
    revision: 1,
  });
  const PREVIEW = {
    host: "GitHub",
    repository: "octo/demo",
    head: "ticket/3",
    base: "main",
    title: "Fix login",
    body: "Fixes the login.\n\nCloses #7",
    account: "aiko",
    login: "aiko-bot",
  };

  it("publishes to the upstream host as the author after showing exactly what is posted", async () => {
    const published = pull({
      upstream: {
        provider: "github",
        repository: "octo/demo",
        account: "aiko",
        number: 42,
        url: "https://github.com/octo/demo/pull/42",
        state: "open",
        pushed: HEAD,
        published_at: "2026-10-16T00:00:00Z",
        error: null,
        stopped: null,
      },
    });
    const fake = fakeApi(
      pullRoutes({
        "GET /api/v1/repos/acme/demo": UPSTREAM_REPO,
        [`GET ${P}`]: [json(200, pull()), json(200, published)],
        [`GET ${P}/publish`]: json(200, PREVIEW),
        [`POST ${P}/publish`]: json(200, published),
      }),
    );
    const user = userEvent.setup();
    renderApp("/acme/demo/-/pulls/3");
    const preview = await screen.findByRole("figure", { name: "What will be posted" });
    expect(within(preview).getByText(/Closes #7/)).toBeInTheDocument();
    expect(within(preview).getByText("Posted on GitHub as aiko-bot.")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Merge (fast-forward)" })).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Publish to GitHub" }));
    await waitFor(() => expect(fake.calls("POST", `${P}/publish`)).toHaveLength(1));
    expect(fake.calls("POST", `${P}/publish`)[0]?.body).toEqual({ expected_head: HEAD, expected_base: BASE });
    expect(await screen.findByRole("link", { name: "octo/demo#42" })).toHaveAttribute("href", "https://github.com/octo/demo/pull/42");
    expect(screen.getByText("Published. People review and merge it on GitHub; Branvil follows.")).toBeInTheDocument();
  });

  it("does not publish when the author has no account there, and says where to set it", async () => {
    fakeApi(
      pullRoutes({
        "GET /api/v1/repos/acme/demo": UPSTREAM_REPO,
        [`GET ${P}/publish`]: json(200, { ...PREVIEW, login: null }),
      }),
    );
    renderApp("/acme/demo/-/pulls/3");
    expect(
      await screen.findByText("aiko has no GitHub account in Branvil; set it under Members."),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Publish to GitHub" })).toBeDisabled();
  });

  it("shows why sending to the host stopped and offers to publish again", async () => {
    fakeApi(
      pullRoutes({
        "GET /api/v1/repos/acme/demo": UPSTREAM_REPO,
        [`GET ${P}`]: json(
          200,
          pull({
            upstream: {
              provider: "github",
              repository: "octo/demo",
              account: "aiko",
              number: 42,
              url: "https://github.com/octo/demo/pull/42",
              state: "open",
              pushed: BASE,
              published_at: "2026-10-16T00:00:00Z",
              error: null,
              stopped: "The host refused the branch ticket/3: [rejected] (stale info).",
            },
          }),
        ),
        [`GET ${P}/publish`]: json(200, PREVIEW),
      }),
    );
    renderApp("/acme/demo/-/pulls/3");
    expect(await screen.findByRole("alert")).toHaveTextContent("Sending stopped: The host refused the branch ticket/3");
    expect(screen.getByRole("button", { name: "Publish again to GitHub" })).toBeEnabled();
  });

  it("shows a check of the upstream host with its page there and its summary, without a Branvil job", async () => {
    const fake = fakeApi(
      pullRoutes({
        "GET /api/v1/repos/acme/demo": UPSTREAM_REPO,
        [`GET ${P}/publish`]: json(200, PREVIEW),
        [`GET ${P}/checks`]: json(200, {
          head_sha: HEAD,
          base_sha: BASE,
          behind_by: 0,
          checks: [
            {
              name: "github/test-windows (remainder)",
              status: "completed",
              conclusion: "failure",
              attempt_id: "github-41",
              details_url: "https://github.com/octo/demo/actions/runs/1/job/41",
              details: "test-windows (remainder): failure",
            },
          ],
          checks_expected: true,
          base_checks: 1,
        }),
        "GET /api/v1/repos/acme/demo/jobs/github-41/log": json(200, { log: "boom on Windows", log_bytes: 15, truncated: false }),
      }),
    );
    const user = userEvent.setup();
    renderApp("/acme/demo/-/pulls/3");
    expect(await screen.findByText("Reason: test-windows (remainder): failure")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Open check github/test-windows (remainder)" }));
    const drawer = await screen.findByRole("dialog", { name: /github\/test-windows/ });
    expect(within(drawer).getByRole("link", { name: "Open on the host" })).toHaveAttribute(
      "href",
      "https://github.com/octo/demo/actions/runs/1/job/41",
    );
    expect(await within(drawer).findByText("boom on Windows")).toBeInTheDocument();
    expect(fake.calls("GET", "/api/v1/repos/acme/demo/jobs/github-41")).toHaveLength(0);
  });

  it("lets the owner edit the title and body in the publish panel, then shows the new preview", async () => {
    const edited = pull({ title: "Fix the login page", body: "Public words.", revision: 2 });
    const fake = fakeApi(
      pullRoutes({
        "GET /api/v1/repos/acme/demo": UPSTREAM_REPO,
        [`GET ${P}`]: [json(200, pull()), json(200, edited)],
        [`GET ${P}/publish`]: [json(200, PREVIEW), json(200, { ...PREVIEW, title: "Fix the login page", body: "Public words." })],
        [`PATCH ${P}`]: json(200, edited),
      }),
    );
    const user = userEvent.setup();
    renderApp("/acme/demo/-/pulls/3");
    await screen.findByRole("figure", { name: "What will be posted" });
    await user.click(screen.getByRole("button", { name: "Edit title and body" }));
    const form = screen.getByRole("form", { name: "Edit title and body" });
    expect(within(form).getByText(/They are Branvil's: GitHub gets them as previewed/)).toBeInTheDocument();
    await user.clear(within(form).getByRole("textbox", { name: "Title" }));
    await user.type(within(form).getByRole("textbox", { name: "Title" }), "Fix the login page");
    await user.clear(within(form).getByRole("textbox", { name: "Body" }));
    await user.type(within(form).getByRole("textbox", { name: "Body" }), "Public words.");
    await user.click(within(form).getByRole("button", { name: "Save" }));
    await waitFor(() => expect(fake.calls("PATCH", P)).toHaveLength(1));
    expect(fake.calls("PATCH", P)[0]?.body).toEqual({ title: "Fix the login page", body: "Public words.", revision: 1 });
    const preview = await screen.findByRole("figure", { name: "What will be posted" });
    expect(await within(preview).findByText("Public words.")).toBeInTheDocument();
    expect(screen.queryByRole("form", { name: "Edit title and body" })).not.toBeInTheDocument();
  });

  it("opens a job's address as its pull request with the job's drawer, and closing it leaves the job's address", async () => {
    fakeApi(
      pullRoutes({
        "GET /api/v1/repos/acme/demo/pulls": json(200, { items: [pull()], next_cursor: null }),
      }),
    );
    const user = userEvent.setup();
    renderApp("/acme/demo/-/jobs/att_1");
    expect(await screen.findByRole("heading", { name: /Fix login/ })).toBeInTheDocument();
    const drawer = await screen.findByRole("dialog", { name: /test/ });
    expect(await within(drawer).findByText("test login ... FAILED")).toBeInTheDocument();
    await user.click(within(drawer).getByRole("button", { name: "Close" }));
    await waitFor(() => expect(window.location.pathname).toBe("/acme/demo/-/pulls/3"));
  });

  it("opens a job of a commit no open pull request has on the commit's page", async () => {
    fakeApi(
      pullRoutes({
        "GET /api/v1/repos/acme/demo/pulls": json(200, { items: [], next_cursor: null }),
        [`GET /api/v1/repos/acme/demo/commits/${HEAD}`]: json(200, { sha: HEAD, message: "Try it", author: "aiko", authored_at: "2026-10-14T10:00:00.000Z", parents: [] }),
      }),
    );
    renderApp("/acme/demo/-/jobs/att_1");
    expect(await screen.findByRole("heading", { name: "Try it" })).toBeInTheDocument();
    expect(await within(await screen.findByRole("dialog", { name: /test/ })).findByText("test login ... FAILED")).toBeInTheDocument();
  });
});
