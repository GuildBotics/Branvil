import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { fakeApi, json, ME, renderApp, SESSION } from "../test/fakeApi";
import { choose } from "../test/mantine";

const page = (items: unknown[]) => json(200, { items, next_cursor: null });

/** What every page reads: the session, the workspaces, the warnings. */
const SHELL = {
  "GET /api/v1/session": json(200, SESSION),
  "GET /api/v1/workspaces": page([{ name: "acme", lanes: [] }]),
  "GET /api/v1/admin/diagnostics": json(200, {}),
};

const REPO = {
  path: "acme/demo",
  default_branch: "main",
  clone_url: "http://127.0.0.1:7878/git/acme/demo.git",
  url: "",
  protected_branches: ["main"],
  required_checks: [],
  labels: ["bug"],
  upstream: null,
  revision: 4,
};

/** Adds `values` to the pill input labelled `label`, one Enter each. */
async function addPills(user: ReturnType<typeof userEvent.setup>, label: string | RegExp, values: string[]) {
  const input = screen.getByLabelText(label, { selector: "input:not([type=hidden])" });
  for (const v of values) await user.type(input, `${v}{Enter}`);
}

describe("settings", () => {
  it("shows the workspace's repositories apart from Branvil's members", async () => {
    fakeApi({
      ...SHELL,
      "GET /api/v1/repos": page([REPO, { ...REPO, path: "beta/x" }]),
    });
    renderApp("/-/settings");
    const sections = await screen.findByRole("navigation", { name: "Settings sections" });
    expect(await within(sections).findByRole("link", { name: "acme" })).toHaveAttribute("aria-current", "page");
    expect(within(sections).getByRole("link", { name: "Members" })).toHaveAttribute("href", "/-/settings/members");
    // Only this workspace's repositories.
    const list = await screen.findByRole("list", { name: "Repositories" });
    expect(within(list).getAllByRole("listitem").map((li) => within(li).getByText(/\//, { selector: "strong" }).textContent)).toEqual(["acme/demo"]);
  });

  it("lists the owner, agents, and runners and creates agents or runners", async () => {
    const aiko = { name: "aiko", display_name: "Aiko", kind: "agent", accounts: [] };
    const fake = fakeApi({
      ...SHELL,
      "GET /api/v1/members": page([ME, aiko]),
      "POST /api/v1/members": json(201, { name: "rin", display_name: "rin", kind: "runner" }),
    });
    const user = userEvent.setup();
    renderApp("/-/settings/members");

    const list = await screen.findByRole("list", { name: "Members" });
    expect(within(list).getAllByRole("listitem").map((li) => li.textContent)).toEqual([
      expect.stringContaining("Owner (owner)"),
      expect.stringContaining("Aiko (aiko)"),
    ]);
    await user.click(screen.getByRole("button", { name: "Create member" }));
    const form = await screen.findByRole("form", { name: "Create member" });
    await user.type(within(form).getByRole("textbox", { name: "Name" }), "rin");
    // Only agents and runners can be created: there is one owner.
    await user.click(within(form).getByRole("combobox", { name: "Kind" }));
    expect((await screen.findAllByRole("option")).map((o) => o.textContent)).toEqual(["agent", "runner"]);
    await user.click(screen.getByRole("option", { name: "runner" }));
    await user.click(within(form).getByRole("button", { name: "Add" }));
    await waitFor(() => expect(fake.calls("POST", "/api/v1/members")).toHaveLength(1));
    expect(fake.calls("POST", "/api/v1/members")[0]?.body).toEqual({ name: "rin", display_name: "", kind: "runner" });
  });

  it("creates a workspace from the sidebar's switcher and shows its board", async () => {
    const fake = fakeApi({
      ...SHELL,
      "GET /api/v1/repos": page([]),
      "POST /api/v1/workspaces": json(201, { name: "beta", lanes: [] }),
      "GET /api/v1/workspaces/beta": json(200, { name: "beta", lanes: ["new"] }),
      "GET /api/v1/workspaces/beta/board/cards": page([]),
    });
    const user = userEvent.setup();
    renderApp("/-/settings");
    await choose(user, "Workspace", "+ New workspace…");
    const dialog = await screen.findByRole("dialog", { name: "New workspace" });
    await user.type(within(dialog).getByRole("textbox", { name: "Name" }), "beta");
    await user.click(within(dialog).getByRole("button", { name: "Create" }));
    await waitFor(() => expect(fake.calls("POST", "/api/v1/workspaces")).toHaveLength(1));
    expect(fake.calls("POST", "/api/v1/workspaces")[0]?.body).toEqual({ name: "beta" });
    await waitFor(() => expect(window.location.pathname).toBe("/beta"));
  });

  it("creates a repository in the shown workspace with the labels its tickets may carry", async () => {
    const fake = fakeApi({
      ...SHELL,
      "GET /api/v1/repos": page([]),
      "POST /api/v1/repos": json(201, {}),
    });
    const user = userEvent.setup();
    renderApp("/-/settings");
    await user.click(await screen.findByRole("button", { name: "Create repository" }));
    const form = await screen.findByRole("form", { name: "Create repository" });
    await user.type(within(form).getByRole("textbox", { name: "Name" }), "demo");
    await addPills(user, "Ticket labels", ["bug", "ui"]);
    await user.click(within(form).getByRole("button", { name: "Add" }));
    await waitFor(() => expect(fake.calls("POST", "/api/v1/repos")).toHaveLength(1));
    expect(fake.calls("POST", "/api/v1/repos")[0]?.body).toEqual({ path: "acme/demo", default_branch: "main", labels: ["bug", "ui"] });
  });

  it("saves a repository's branches, required checks, and labels with the revision the form was filled from", async () => {
    const fake = fakeApi({
      ...SHELL,
      "GET /api/v1/repos": page([REPO]),
      "PATCH /api/v1/repos/acme/demo": json(200, { ...REPO, required_checks: ["test", "lint"], revision: 5 }),
    });
    const user = userEvent.setup();
    renderApp("/-/settings");
    await user.click(await screen.findByRole("button", { name: "Edit" }));
    expect(screen.getByText("http://127.0.0.1:7878/git/acme/demo.git")).toBeInTheDocument();
    await addPills(user, "Required checks", ["test", "lint"]);
    await user.click(screen.getByRole("tab", { name: "Labels" }));
    await addPills(user, "Ticket labels", ["ui"]);
    await user.click(screen.getAllByRole("button", { name: "Save" })[0] as HTMLElement);
    await waitFor(() => expect(fake.calls("PATCH", "/api/v1/repos/acme/demo")).toHaveLength(1));
    expect(fake.calls("PATCH", "/api/v1/repos/acme/demo")[0]?.body).toEqual({
      revision: 4,
      default_branch: "main",
      protected_branches: ["main"],
      required_checks: ["test", "lint"],
      labels: ["bug", "ui"],
    });
  });

  it("sets an agent's code host account with a token that is sent once and never shown", async () => {
    const aiko = { name: "aiko", display_name: "Aiko", kind: "agent", accounts: [] };
    const withAccount = { ...aiko, accounts: [{ provider: "github", login: "aiko-bot", kind: "token", set_at: "2026-10-16T00:00:00Z" }] };
    const fake = fakeApi({
      ...SHELL,
      "GET /api/v1/members": [page([ME, aiko]), page([ME, withAccount])],
      "PUT /api/v1/members/aiko/accounts/github": json(200, withAccount),
      "GET /api/v1/members/aiko/tokens": page([]),
    });
    const user = userEvent.setup();
    renderApp("/-/settings/members");
    const row = (await screen.findByText("Aiko (aiko)")).closest("[role=listitem]") as HTMLElement;
    await user.click(within(row).getByRole("button", { name: "Edit" }));
    await user.click(within(row).getByRole("tab", { name: "Accounts on code hosts" }));
    await user.type(within(row).getByLabelText("GitHub token"), "gh-secret");
    await user.click(within(row).getByRole("button", { name: "Save account" }));
    await waitFor(() => expect(fake.calls("PUT", "/api/v1/members/aiko/accounts/github")).toHaveLength(1));
    expect(fake.calls("PUT", "/api/v1/members/aiko/accounts/github")[0]?.body).toEqual({ token: "gh-secret" });
    expect((await within(row).findAllByText("aiko-bot", { exact: false })).length).toBeGreaterThan(0);
    expect(screen.queryByDisplayValue("gh-secret")).not.toBeInTheDocument();
  });

  it("sets a member's picture from a file, shows it wherever the member appears, and removes it", async () => {
    const aiko = { name: "aiko", display_name: "Aiko", kind: "agent", accounts: [], avatar_url: null };
    const pictured = { ...aiko, avatar_url: "/api/v1/members/aiko/avatar?v=1234" };
    const fake = fakeApi({
      ...SHELL,
      "GET /api/v1/members": [page([ME, aiko]), page([ME, pictured]), page([ME, aiko])],
      "PUT /api/v1/members/aiko/avatar": json(200, pictured),
      "DELETE /api/v1/members/aiko/avatar": json(200, aiko),
      "GET /api/v1/members/aiko/tokens": page([]),
    });
    const user = userEvent.setup();
    renderApp("/-/settings/members");
    const row = (await screen.findByText("Aiko (aiko)")).closest("[role=listitem]") as HTMLElement;
    expect(within(row).queryByRole("img")).not.toBeInTheDocument();
    await user.click(within(row).getByRole("button", { name: "Edit" }));
    expect(within(row).queryByRole("button", { name: "Remove the picture" })).not.toBeInTheDocument();
    const file = new File([new Uint8Array([0x89, 0x50, 0x4e, 0x47])], "aiko.png", { type: "image/png" });
    await user.upload(row.querySelector("input[type=file]") as HTMLInputElement, file);
    await waitFor(() => expect(fake.calls("PUT", "/api/v1/members/aiko/avatar")).toHaveLength(1));
    const sent = fake.calls("PUT", "/api/v1/members/aiko/avatar")[0];
    expect(sent?.body).toBe(file);
    expect(sent?.headers["Content-Type"]).toBe("image/png");
    // Every avatar of the member (the row's and the editor's) shows the picture.
    await waitFor(() => expect(within(row).getAllByRole("img").map((i) => i.getAttribute("src"))).toEqual([pictured.avatar_url, pictured.avatar_url]));

    await user.click(within(row).getByRole("button", { name: "Remove the picture" }));
    await waitFor(() => expect(fake.calls("DELETE", "/api/v1/members/aiko/avatar")).toHaveLength(1));
    await waitFor(() => expect(within(row).queryByRole("img")).not.toBeInTheDocument());
  });

  it("switches an account between a token and a GitHub App and sends each form's credential", async () => {
    const aiko = { name: "aiko", display_name: "Aiko", kind: "agent", accounts: [{ provider: "github", login: "aiko-bot", kind: "token", set_at: "2026-10-16T00:00:00Z" }] };
    const asApp = { ...aiko, accounts: [{ provider: "github", login: "octo-app[bot]", kind: "app", set_at: "2026-10-16T01:00:00Z" }] };
    const fake = fakeApi({
      ...SHELL,
      "GET /api/v1/members": [page([ME, aiko]), page([ME, asApp])],
      "PUT /api/v1/members/aiko/accounts/github": json(200, asApp),
      "GET /api/v1/members/aiko/tokens": page([]),
    });
    const user = userEvent.setup();
    renderApp("/-/settings/members");
    const row = (await screen.findByText("Aiko (aiko)")).closest("[role=listitem]") as HTMLElement;
    await user.click(within(row).getByRole("button", { name: "Edit" }));
    await user.click(within(row).getByRole("tab", { name: "Accounts on code hosts" }));
    // The account's kind is shown and the form starts on it.
    expect(within(row).getAllByText("Token").length).toBeGreaterThan(0);
    expect(within(row).getByRole("radio", { name: "Token" })).toBeChecked();
    expect(within(row).getByLabelText("Replace the GitHub token")).toBeInTheDocument();

    // The app form: nothing is sent until every field is filled.
    await user.click(within(row).getByRole("radio", { name: "GitHub App" }));
    expect(within(row).queryByLabelText("Replace the GitHub token")).not.toBeInTheDocument();
    const save = within(row).getByRole("button", { name: "Save account" });
    await user.type(within(row).getByLabelText("App ID"), " 4242 ");
    await user.type(within(row).getByLabelText("Installation ID"), "777");
    expect(save).toBeDisabled();
    // The key comes from a chosen file (or is pasted).
    const pem = "-----BEGIN RSA PRIVATE KEY-----\nMIIE\n-----END RSA PRIVATE KEY-----\n";
    await user.upload(within(row).getByLabelText("Choose the key file"), new File([pem], "app.pem"));
    await waitFor(() => expect(within(row).getByLabelText("Private key (PEM)")).toHaveValue(pem));
    await user.click(save);
    await waitFor(() => expect(fake.calls("PUT", "/api/v1/members/aiko/accounts/github")).toHaveLength(1));
    expect(fake.calls("PUT", "/api/v1/members/aiko/accounts/github")[0]?.body).toEqual({
      app: { app_id: "4242", installation_id: "777", private_key: pem },
    });
    // Saved: the login is the app's bot, and the key is gone from the page.
    expect((await within(row).findAllByText("octo-app[bot]")).length).toBeGreaterThan(0);
    expect(within(row).getByLabelText("Private key (PEM)")).toHaveValue("");
    expect(screen.queryByDisplayValue(pem)).not.toBeInTheDocument();

    // Back to a token: the token form sends only the token.
    await user.click(within(row).getByRole("radio", { name: "Token" }));
    await user.type(within(row).getByLabelText("Replace the GitHub token"), "gh-new");
    await user.click(within(row).getByRole("button", { name: "Save account" }));
    await waitFor(() => expect(fake.calls("PUT", "/api/v1/members/aiko/accounts/github")).toHaveLength(2));
    expect(fake.calls("PUT", "/api/v1/members/aiko/accounts/github")[1]?.body).toEqual({ token: "gh-new" });
  });

  it("sets a repository's upstream, shows what following found and who reads it, and says when nobody can read it", async () => {
    const repo = { ...REPO, labels: [] };
    const following = {
      ...repo,
      upstream: {
        provider: "github",
        host: "GitHub",
        repository: "octo/demo",
        url: "https://github.com/octo/demo",
        reader: { member: "aiko", chosen: false },
        following: { tip: "a".repeat(40), checked_at: "2026-10-16T00:00:00Z", followed_at: null, error: null, stopped: null },
      },
      revision: 5,
    };
    const account = { provider: "github", login: "x", set_at: "2026-10-16T00:00:00Z" };
    const kenReads = { ...following, upstream: { ...following.upstream, reader: { member: "ken", chosen: true } }, revision: 6 };
    const fake = fakeApi({
      ...SHELL,
      "GET /api/v1/repos": [page([repo]), page([following]), page([following]), page([kenReads])],
      "GET /api/v1/members": page([
        { name: "aiko", display_name: "Aiko", kind: "agent", accounts: [account] },
        { name: "ken", display_name: "Ken", kind: "agent", accounts: [account] },
        { name: "mia", display_name: "Mia", kind: "agent", accounts: [] },
      ]),
      "PUT /api/v1/repos/acme/demo/upstream": [
        { status: 409, body: { error: { code: "no_account", message: "No member has an account." } } },
        json(200, following),
        json(200, kenReads),
      ],
    });
    const user = userEvent.setup();
    renderApp("/-/settings");
    await user.click(await screen.findByRole("button", { name: "Edit" }));
    await user.click(screen.getByRole("tab", { name: "Upstream" }));
    expect(screen.getByText("No upstream: pull requests are merged here.")).toBeInTheDocument();
    const form = screen.getByRole("form", { name: "Set upstream" });
    await user.type(within(form).getByRole("textbox", { name: "Repository there" }), "octo/demo");
    await user.click(within(form).getByRole("button", { name: "Set upstream" }));
    expect(
      await screen.findByText("Set a member's GitHub account under Members first (an agent's bot account is enough): it reads the upstream."),
    ).toBeInTheDocument();
    await user.click(within(form).getByRole("button", { name: "Set upstream" }));
    await waitFor(() => expect(fake.calls("PUT", "/api/v1/repos/acme/demo/upstream")).toHaveLength(2));
    expect(fake.calls("PUT", "/api/v1/repos/acme/demo/upstream")[1]?.body).toEqual({ provider: "github", repository: "octo/demo" });
    expect(await screen.findByRole("link", { name: "GitHub octo/demo" })).toHaveAttribute("href", "https://github.com/octo/demo");
    expect(screen.getByText("main follows")).toBeInTheDocument();
    await user.click(screen.getByRole("tab", { name: "Basics" }));
    expect(screen.getByRole("textbox", { name: "Default branch" })).toHaveAttribute("readonly");
    await user.click(screen.getByRole("tab", { name: "Upstream" }));

    // Who reads it, and changing it explicitly (only members with an account are offered).
    expect(screen.getByText("aiko", { selector: "strong" })).toBeInTheDocument();
    expect(screen.getByText("(picked)")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Change" })).toBeDisabled();
    await user.click(screen.getByRole("combobox", { name: "Change the reading account" }));
    expect((await screen.findAllByRole("option")).map((o) => o.textContent)).toEqual(["aiko (x)", "ken (x)", "Pick again"]);
    await user.click(screen.getByRole("option", { name: "ken (x)" }));
    await user.click(screen.getByRole("button", { name: "Change" }));
    await waitFor(() => expect(fake.calls("PUT", "/api/v1/repos/acme/demo/upstream")).toHaveLength(3));
    expect(fake.calls("PUT", "/api/v1/repos/acme/demo/upstream")[2]?.body).toEqual({
      provider: "github",
      repository: "octo/demo",
      reader: "ken",
    });
    expect(await screen.findByText("(chosen)")).toBeInTheDocument();
  });
});
