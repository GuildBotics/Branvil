import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { apiError, fakeApi, json, renderApp, SESSION } from "../test/fakeApi";
import { choose, pickFromMenu } from "../test/mantine";
import { ticketNamed } from "../views/ticket/Relations";
import { card } from "./fixtures";

const TICKET = "/api/v1/repos/acme/demo/tickets/1";
const at = "2026-10-14T10:00:00.000Z";
const REPO = {
  path: "acme/demo",
  default_branch: "main",
  clone_url: "",
  url: "",
  protected_branches: [],
  required_checks: [],
  labels: ["bug", "docs"],
  upstream: null,
  revision: 1,
};
const EMPTY = json(200, { items: [], next_cursor: null });

/** The routes every ticket page reads, with `extra` over them. */
function ticketRoutes(extra: Record<string, Parameters<typeof fakeApi>[0][string]>) {
  return {
    "GET /api/v1/session": json(200, SESSION),
    "GET /api/v1/workspaces": json(200, { items: [{ name: "acme", lanes: [] }], next_cursor: null }),
    "GET /api/v1/admin/diagnostics": json(200, {}),
    "GET /api/v1/repos/acme/demo": json(200, REPO),
    "GET /api/v1/members": EMPTY,
    [`GET ${TICKET}/comments`]: EMPTY,
    [`GET ${TICKET}/pulls`]: EMPTY,
    ...extra,
  };
}

describe("ticket view", () => {
  it("chooses one assignee from the avatar menu, replacing the current one, and clears it", async () => {
    const fake = fakeApi(
      ticketRoutes({
        "GET /api/v1/members": json(200, {
          items: [
            { name: "aiko", display_name: "Aiko", kind: "agent", accounts: [], reviewers: [] },
            { name: "mia", display_name: "Mia", kind: "agent", accounts: [], reviewers: [] },
            { name: "ci", display_name: "", kind: "runner", accounts: [], reviewers: [] },
          ],
          next_cursor: null,
        }),
        [`GET ${TICKET}`]: [
          json(200, card("1", "new", { assignee: { name: "aiko", assigned_at: at } })),
          json(200, card("1", "new", { assignee: { name: "mia", assigned_at: at } })),
          json(200, card("1", "new")),
        ],
        [`PUT ${TICKET}/assignee`]: json(200, {}),
        [`DELETE ${TICKET}/assignee`]: json(200, {}),
      }),
    );
    const user = userEvent.setup();
    renderApp("/acme/demo/-/tickets/1");
    expect(await screen.findByRole("button", { name: "Assignee: aiko — click to change" })).toBeInTheDocument();
    // Within its repository a ticket reads #1; the way back is the board.
    expect(screen.getByRole("heading", { level: 2 })).toHaveTextContent("#1 Title of 1");
    // (The sidebar's Board link is the other one.)
    expect(screen.getAllByRole("link", { name: "Task board" }).map((a) => a.getAttribute("href"))).toEqual(["/acme", "/acme"]);

    await user.click(screen.getByRole("button", { name: "Assignee: aiko — click to change" }));
    // Runners take no tickets.
    expect(screen.queryByRole("menuitem", { name: "ci" })).not.toBeInTheDocument();
    await user.click(await screen.findByRole("menuitem", { name: "mia" }));
    await waitFor(() => expect(fake.calls("PUT", `${TICKET}/assignee`)).toHaveLength(1));
    expect(fake.calls("PUT", `${TICKET}/assignee`)[0]?.body).toEqual({ name: "mia" });

    await pickFromMenu(user, "Assignee: mia — click to change", "No one");
    await waitFor(() => expect(fake.calls("DELETE", `${TICKET}/assignee`)).toHaveLength(1));
    expect(await screen.findByRole("button", { name: "Assignee: No one — click to change" })).toBeInTheDocument();
  });

  it("shows reactions as emoji, names them on hover, and takes back only the signed-in member's own", async () => {
    const comment = { id: "cmt_1", author: "ken", body: "Looks good", meta: null, created_at: at, url: "" };
    const reactions = [
      { author: "aiko", reaction: "ack" },
      { author: "ken", reaction: "ack" },
      { author: "owner", reaction: "agree" },
    ];
    const fake = fakeApi(
      ticketRoutes({
        [`GET ${TICKET}`]: json(200, card("1", "new")),
        [`GET ${TICKET}/comments`]: json(200, { items: [{ ...comment, reactions }], next_cursor: null }),
        [`POST ${TICKET}/reactions`]: json(200, {}),
        [`DELETE ${TICKET}/reactions`]: json(204, null),
      }),
    );
    const user = userEvent.setup();
    renderApp("/acme/demo/-/tickets/1");
    const ack = await screen.findByRole("button", { name: "Ack (2)" });
    const agree = screen.getByRole("button", { name: "Agree (1)" });
    expect(ack).toHaveTextContent("👀 2");
    expect(ack).toHaveAttribute("aria-pressed", "false");
    expect(agree).toHaveAttribute("aria-pressed", "true");
    await user.hover(ack);
    expect(await screen.findByRole("tooltip")).toHaveTextContent("Ack: aiko, ken");
    await user.unhover(ack);
    await user.hover(screen.getByRole("button", { name: "Add a reaction" }));
    expect(await screen.findByRole("tooltip", { name: "Add a reaction" })).toBeInTheDocument();

    // Someone else's reaction is joined; one's own is taken back.
    await user.click(ack);
    await user.click(agree);
    await waitFor(() => expect(fake.calls("DELETE", `${TICKET}/reactions`)).toHaveLength(1));
    expect(fake.calls("POST", `${TICKET}/reactions`)[0]?.body).toEqual({ target_id: "cmt_1", reaction: "ack" });
    expect(fake.calls("DELETE", `${TICKET}/reactions`)[0]?.body).toEqual({ target_id: "cmt_1", reaction: "agree" });

    // The menu offers every emoji by name, and choosing one's own again takes it back too.
    await user.click(screen.getByRole("button", { name: "Add a reaction" }));
    const items = await screen.findAllByRole("menuitem");
    expect(items.map((i) => [i.getAttribute("aria-label"), i.textContent])).toEqual([
      ["Ack", "👀"],
      ["Agree", "👍"],
      ["Celebrate", "🎉"],
      ["Support", "❤️"],
    ]);
    await user.click(screen.getByRole("menuitem", { name: "Agree" }));
    await waitFor(() => expect(fake.calls("DELETE", `${TICKET}/reactions`)).toHaveLength(2));
    await user.click(screen.getByRole("button", { name: "Add a reaction" }));
    await user.click(await screen.findByRole("menuitem", { name: "Celebrate" }));
    await waitFor(() => expect(fake.calls("POST", `${TICKET}/reactions`)).toHaveLength(2));
    expect(fake.calls("POST", `${TICKET}/reactions`)[1]?.body).toEqual({ target_id: "cmt_1", reaction: "celebrate" });
  });

  it("moves a ticket between the four lanes of its workspace's board, which it is never off", async () => {
    const fake = fakeApi(
      ticketRoutes({
        [`GET ${TICKET}`]: [json(200, card("1", "new")), json(200, card("1", "ready")), json(200, card("1", "done"))],
        [`PUT ${TICKET}/board`]: json(200, {}),
      }),
    );
    const user = userEvent.setup();
    renderApp("/acme/demo/-/tickets/1");
    const select = await screen.findByRole("combobox", { name: "Lane" });
    expect(select).toHaveValue("New");

    await user.click(select);
    const options = await screen.findAllByRole("option");
    expect(options.map((o) => o.textContent)).toEqual(["New", "Ready", "In progress", "Done"]);
    await user.click(screen.getByRole("option", { name: "Ready" }));
    await waitFor(() => expect(screen.getByRole("combobox", { name: "Lane" })).toHaveValue("Ready"));
    await choose(user, "Lane", "Done");
    await waitFor(() => expect(fake.calls("PUT", `${TICKET}/board`)).toHaveLength(2));
    expect(fake.calls("PUT", `${TICKET}/board`).map((c) => c.body)).toEqual([{ lane: "ready" }, { lane: "done" }]);
  });

  it("edits the title and the body in place, each with the revision it was read at", async () => {
    const fake = fakeApi(
      ticketRoutes({
        [`GET ${TICKET}`]: json(200, card("1", "new", { body: "Old body", revision: 4 })),
        [`PATCH ${TICKET}`]: [
          json(200, { ticket: card("1", "new", { title: "New title", body: "Old body", revision: 5 }), state_changed: false }),
          apiError(409, "conflict", "Changed."),
        ],
      }),
    );
    const user = userEvent.setup();
    renderApp("/acme/demo/-/tickets/1");

    await user.click(await screen.findByRole("heading", { name: "#1 Title of 1" }));
    const title = screen.getByRole("textbox", { name: "Title" });
    await user.clear(title);
    await user.type(title, "New title{Enter}");
    await waitFor(() => expect(fake.calls("PATCH", TICKET)).toHaveLength(1));
    expect(fake.calls("PATCH", TICKET)[0]?.body).toEqual({ title: "New title", revision: 4 });

    await user.click(screen.getByRole("button", { name: "Edit the body" }));
    const body = screen.getByRole("textbox", { name: "Description" });
    expect(body).toHaveValue("Old body");
    await user.clear(body);
    await user.type(body, "New body");
    await user.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(fake.calls("PATCH", TICKET)).toHaveLength(2));
    expect(fake.calls("PATCH", TICKET)[1]?.body).toEqual({ body: "New body", revision: 4 });
    // A change by someone else meanwhile is refused and said; the text stays to be redone.
    expect(await screen.findByRole("alert")).toHaveTextContent("The ticket was changed by someone else");
    expect(screen.getByRole("textbox", { name: "Description" })).toHaveValue("New body");
  });

  it("closes as completed, or as not planned from the close menu, and reopens", async () => {
    const fake = fakeApi(
      ticketRoutes({
        [`GET ${TICKET}`]: [
          json(200, card("1", "new")),
          json(200, card("1", "new")),
          json(200, card("1", "new", { state: "closed", state_reason: "not_planned" })),
        ],
        [`PATCH ${TICKET}`]: [
          json(200, { ticket: card("1", "new"), state_changed: true }),
          json(200, { ticket: card("1", "new", { state: "closed", state_reason: "not_planned" }), state_changed: true }),
          json(200, { ticket: card("1", "new"), state_changed: true }),
        ],
      }),
    );
    const user = userEvent.setup();
    renderApp("/acme/demo/-/tickets/1");
    await user.click(await screen.findByRole("button", { name: "Close" }));
    await waitFor(() => expect(fake.calls("PATCH", TICKET)).toHaveLength(1));
    await pickFromMenu(user, "Close with a reason", "Close as not planned");
    await waitFor(() => expect(fake.calls("PATCH", TICKET)).toHaveLength(2));
    await user.click(await screen.findByRole("button", { name: "Reopen" }));
    await waitFor(() => expect(fake.calls("PATCH", TICKET)).toHaveLength(3));
    expect(fake.calls("PATCH", TICKET).map((c) => c.body)).toEqual([
      { state: "closed", state_reason: "completed" },
      { state: "closed", state_reason: "not_planned" },
      { state: "open", state_reason: null },
    ]);
  });

  it("toggles the repository's labels on the ticket", async () => {
    const fake = fakeApi(
      ticketRoutes({
        [`GET ${TICKET}`]: json(200, card("1", "new", { labels: ["bug"] })),
        [`PATCH ${TICKET}`]: json(200, { ticket: card("1", "new"), state_changed: false }),
      }),
    );
    const user = userEvent.setup();
    renderApp("/acme/demo/-/tickets/1");
    const labels = await screen.findByRole("group", { name: "Labels" });
    expect(within(labels).getByRole("checkbox", { name: "bug" })).toBeChecked();
    await user.click(within(labels).getByRole("checkbox", { name: "docs" }));
    await user.click(within(labels).getByRole("checkbox", { name: "bug" }));
    await waitFor(() => expect(fake.calls("PATCH", TICKET)).toHaveLength(2));
    expect(fake.calls("PATCH", TICKET).map((c) => c.body)).toEqual([{ add_labels: ["docs"] }, { remove_labels: ["bug"] }]);
  });

  it("names the upstream issue a ticket stands for", async () => {
    const issue = "https://github.com/octo/demo/issues/7";
    const fake = fakeApi(
      ticketRoutes({
        "GET /api/v1/repos/acme/demo": json(200, {
          ...REPO,
          upstream: {
            provider: "github",
            host: "GitHub",
            repository: "octo/demo",
            url: "https://github.com/octo/demo",
            reader: { member: "aiko", chosen: false },
            following: { tip: null, checked_at: null, followed_at: null, error: null, stopped: null },
          },
        }),
        [`GET ${TICKET}`]: [json(200, card("1", "new")), json(200, card("1", "new", { upstream_issue: issue }))],
        [`PATCH ${TICKET}`]: json(200, { ticket: card("1", "new", { upstream_issue: issue }), state_changed: false }),
      }),
    );
    const user = userEvent.setup();
    renderApp("/acme/demo/-/tickets/1");
    expect(await screen.findByText("GitHub issue")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Edit the issue URL" }));
    await user.type(screen.getByLabelText("Issue URL"), issue);
    await user.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(fake.calls("PATCH", TICKET)).toHaveLength(1));
    expect(fake.calls("PATCH", TICKET)[0]?.body).toEqual({ upstream_issue: issue });
    expect(await screen.findByRole("link", { name: "octo/demo#7" })).toHaveAttribute("href", issue);
  });

  it("shows the relations from both sides and changes each on the ticket that stores it", async () => {
    const ref = (key: string, state: "open" | "closed" = "open", repository = "acme/demo") => ({
      ...card(key, "new", { repository }),
      state,
      state_reason: null,
    });
    const shown = card("1", "new", {
      parent: ref("9"),
      children: [ref("2", "closed"), ref("3")],
      blocked_by: [ref("4", "open", "acme/web")],
      blocks: [],
    });
    const done = json(200, { ticket: shown, state_changed: false });
    const fake = fakeApi(
      ticketRoutes({
        [`GET ${TICKET}`]: json(200, shown),
        [`PATCH ${TICKET}`]: done,
        "PATCH /api/v1/repos/acme/demo/tickets/3": done,
        "PATCH /api/v1/repos/acme/demo/tickets/5": done,
        "PATCH /api/v1/repos/acme/web/tickets/6": done,
      }),
    );
    const user = userEvent.setup();
    renderApp("/acme/demo/-/tickets/1");
    const group = async (name: string) => within(await screen.findByRole("group", { name }));
    expect((await group("Parent ticket")).getByRole("link", { name: "#9 Title of 9" })).toHaveAttribute("href", "/acme/demo/-/tickets/9");
    // A parent is set already: only taking it off is offered.
    expect((await group("Parent ticket")).queryByRole("button", { name: "Add to Parent ticket" })).not.toBeInTheDocument();
    expect((await group("Child tickets")).getByText("1/2 closed")).toBeInTheDocument();
    expect((await group("Blocked by")).getByRole("link", { name: "acme/web#4 Title of 4" })).toBeInTheDocument();
    expect(screen.getByText("It can be moved to In progress once every ticket it is blocked by is closed.")).toBeInTheDocument();
    expect((await group("Blocks")).getByText("none")).toBeInTheDocument();

    // A child names its parent: adding and removing one writes the child.
    await user.click((await group("Child tickets")).getByRole("button", { name: "Add to Child tickets" }));
    await user.type(screen.getByLabelText("Ticket to add to Child tickets"), "nope");
    await user.click(screen.getByRole("button", { name: "Add" }));
    expect(await screen.findByText("Name a ticket as #12, repository#12, or its URL.")).toBeInTheDocument();
    await user.clear(screen.getByLabelText("Ticket to add to Child tickets"));
    await user.type(screen.getByLabelText("Ticket to add to Child tickets"), "#5");
    await user.click(screen.getByRole("button", { name: "Add" }));
    await waitFor(() => expect(fake.calls("PATCH", "/api/v1/repos/acme/demo/tickets/5")).toHaveLength(1));
    expect(fake.calls("PATCH", "/api/v1/repos/acme/demo/tickets/5")[0]?.body).toEqual({ parent: shown.url });
    await user.click(screen.getByRole("button", { name: "Remove #3 from Child tickets" }));
    await waitFor(() => expect(fake.calls("PATCH", "/api/v1/repos/acme/demo/tickets/3")).toHaveLength(1));
    expect(fake.calls("PATCH", "/api/v1/repos/acme/demo/tickets/3")[0]?.body).toEqual({ parent: null });

    // This ticket names its parent and its blockers; a ticket it blocks names it.
    await user.click(screen.getByRole("button", { name: "Remove #9 from Parent ticket" }));
    await user.click(screen.getByRole("button", { name: "Remove acme/web#4 from Blocked by" }));
    await waitFor(() => expect(fake.calls("PATCH", TICKET)).toHaveLength(2));
    expect(fake.calls("PATCH", TICKET).map((c) => c.body)).toEqual([{ parent: null }, { remove_blocked_by: [shown.blocked_by[0]?.url] }]);
    await user.click((await group("Blocks")).getByRole("button", { name: "Add to Blocks" }));
    await user.type(screen.getByLabelText("Ticket to add to Blocks"), "acme/web#6");
    await user.click(screen.getByRole("button", { name: "Add" }));
    await waitFor(() => expect(fake.calls("PATCH", "/api/v1/repos/acme/web/tickets/6")).toHaveLength(1));
    expect(fake.calls("PATCH", "/api/v1/repos/acme/web/tickets/6")[0]?.body).toEqual({ add_blocked_by: [shown.url] });
  });

  it("says why a parent with open children is not closed as completed", async () => {
    fakeApi(
      ticketRoutes({
        [`GET ${TICKET}`]: json(200, card("1", "new")),
        [`PATCH ${TICKET}`]: apiError(409, "children_open", "Ticket 1 has open child tickets."),
      }),
    );
    const user = userEvent.setup();
    renderApp("/acme/demo/-/tickets/1");
    await user.click(await screen.findByRole("button", { name: "Close" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("It can be closed as completed once every child ticket is closed.");
  });
});

describe("naming a related ticket", () => {
  const ticket = card("1", "new");
  const base = window.location.origin;
  it("takes #number, repository#number, or a URL of this Branvil", () => {
    expect(ticketNamed(" #12 ", ticket)).toEqual({ repo: "acme/demo", key: "12", url: `${base}/acme/demo/-/tickets/12` });
    expect(ticketNamed("12", ticket)?.key).toBe("12");
    expect(ticketNamed("acme/web#3", ticket)).toEqual({ repo: "acme/web", key: "3", url: `${base}/acme/web/-/tickets/3` });
    expect(ticketNamed(`${base}/acme/web/-/tickets/3`, ticket)?.url).toBe(`${base}/acme/web/-/tickets/3`);
  });
  it("names nothing else", () => {
    for (const text of ["", "#0", "#012", "acme/demo#", "https://github.com/acme/demo/issues/3", `${base}/acme/demo/-/pulls/3`, "acme demo#3"]) {
      expect(ticketNamed(text, ticket), text).toBeNull();
    }
  });
});
