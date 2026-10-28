import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { fakeApi, json, renderApp, SESSION } from "../test/fakeApi";
import { choose } from "../test/mantine";
import { card } from "./fixtures";

const page = (items: unknown[]) => json(200, { items, next_cursor: null });
const workspace = (name: string) => ({ name, lanes: ["new"] });

function setup(empty = false) {
  const routes: Parameters<typeof fakeApi>[0] = {
    "GET /api/v1/session": json(200, SESSION),
    "GET /api/v1/workspaces": page(empty ? [] : [workspace("acme"), workspace("beta")]),
    "GET /api/v1/admin/diagnostics": json(200, {}),
    "GET /api/v1/members": page([]),
    "GET /api/v1/repos": page([]),
    "GET /api/v1/repos/acme/demo/tickets/1": json(200, card("1", "new")),
    "POST /api/v1/workspaces": json(201, workspace("gamma")),
  };
  for (const name of ["acme", "beta", "gamma"]) {
    routes[`GET /api/v1/workspaces/${name}`] = json(200, workspace(name));
    routes[`GET /api/v1/workspaces/${name}/board/cards`] = page([]);
  }
  return fakeApi(routes);
}

async function expectStart(name: string, showBoard: boolean) {
  await waitFor(() => expect(window.location.pathname).toBe(showBoard ? `/${name}` : `/${name}/-/relations`));
  const nav = await screen.findByRole("navigation", { name: "Main" });
  expect(await within(nav).findByRole("link", { name: showBoard ? "Task board" : "Task flow" })).toHaveAttribute("aria-current", "page");
  if (!showBoard) expect(within(nav).queryByRole("link", { name: "Task board" })).not.toBeInTheDocument();
}

describe.each([true, false])("workspace navigation (showBoard=%s)", (showBoard) => {
  it.each(["home", "switch", "create from sidebar", "create from home"] as const)("uses the display preference for %s", async (entry) => {
    const fake = setup(entry === "create from home");
    window.localStorage.setItem("branvil.sidebar.showBoard", String(showBoard));
    window.localStorage.setItem("branvil.workspace", "beta");
    const user = userEvent.setup();
    const historyLength = window.history.length;
    renderApp(entry === "home" || entry === "create from home" ? "/" : entry === "switch" ? "/acme" : "/-/settings/members");

    if (entry === "switch") {
      await choose(user, "Workspace", "beta");
    } else if (entry.startsWith("create")) {
      if (entry === "create from sidebar") await choose(user, "Workspace", "+ New workspace…");
      else await user.click(await screen.findByRole("button", { name: "New workspace" }));
      const dialog = await screen.findByRole("dialog", { name: "New workspace" });
      await user.type(within(dialog).getByRole("textbox", { name: "Name" }), "gamma");
      await user.click(within(dialog).getByRole("button", { name: "Create" }));
      await waitFor(() => expect(fake.calls("POST", "/api/v1/workspaces")).toHaveLength(1));
    }
    const target = entry.startsWith("create") ? "gamma" : "beta";
    await expectStart(target, showBoard);
    expect(window.localStorage.getItem("branvil.workspace")).toBe(target);
    if (entry === "home") expect(window.history.length).toBe(historyLength);
  });

  it("selects the visible ticket navigation and uses it when changing workspace", async () => {
    setup();
    window.localStorage.setItem("branvil.sidebar.showBoard", String(showBoard));
    const user = userEvent.setup();
    renderApp("/acme/demo/-/tickets/1");
    const nav = await screen.findByRole("navigation", { name: "Main" });
    expect(within(nav).getByRole("link", { name: showBoard ? "Task board" : "Task flow" })).toHaveAttribute("aria-current", "page");
    await choose(user, "Workspace", "beta");
    await expectStart("beta", showBoard);
  });

  it.each(["pulls", "relations", "settings"] as const)("keeps the %s page when changing workspace", async (section) => {
    setup();
    window.localStorage.setItem("branvil.sidebar.showBoard", String(showBoard));
    renderApp(section === "settings" ? "/-/settings/members" : `/acme/-/${section}`);
    await choose(userEvent.setup(), "Workspace", "beta");
    await waitFor(() => expect(window.location.pathname).toBe(section === "settings" ? "/-/settings" : `/beta/-/${section}`));
  });
});

it("keeps direct board URLs usable with the sidebar link hidden", async () => {
  setup();
  window.localStorage.setItem("branvil.sidebar.showBoard", "false");
  renderApp("/acme");
  expect(await screen.findByText("acme · 0 open")).toBeInTheDocument();
  expect(window.location.pathname).toBe("/acme");
  expect(within(screen.getByRole("navigation", { name: "Main" })).queryByRole("link", { name: "Task board" })).not.toBeInTheDocument();
});

it("uses the live preference when browser storage cannot save it", async () => {
  setup();
  const save = vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => { throw new Error("Storage disabled"); });
  try {
    const user = userEvent.setup();
    renderApp("/-/settings/display");
    await user.click(await screen.findByRole("switch", { name: "Show the task board in the sidebar" }));
    await choose(user, "Workspace", "+ New workspace…");
    const dialog = await screen.findByRole("dialog", { name: "New workspace" });
    await user.type(within(dialog).getByRole("textbox", { name: "Name" }), "gamma");
    await user.click(within(dialog).getByRole("button", { name: "Create" }));
    await expectStart("gamma", false);
  } finally {
    save.mockRestore();
  }
});
