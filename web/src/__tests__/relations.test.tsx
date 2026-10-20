import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { Card, TicketRef } from "../api/types";
import { apiError, fakeApi, json, renderApp, SESSION } from "../test/fakeApi";
import { pickFromMenu } from "../test/mantine";
import { card } from "./fixtures";

const WORKSPACE = { name: "acme", lanes: ["new", "ready", "in_progress", "done"] };

const MEMBERS = {
  items: [
    { name: "owner", display_name: "Owner", kind: "owner", accounts: [] },
    { name: "aiko", display_name: "Aiko", kind: "agent", accounts: [] },
  ],
  next_cursor: null,
};

const url = (key: string) => `${window.location.origin}/acme/demo/-/tickets/${key}`;
const ref = (key: string, state: TicketRef["state"] = "open"): TicketRef => ({
  url: url(key),
  repository: "acme/demo",
  key,
  title: `Title of ${key}`,
  state,
  state_reason: null,
});

/**
 * #1 is the parent of #2 and #3, #2 waits for #4, #5 has no relation.
 * Collapsed, #1 stands for #2: the line from #4 is dotted.
 */
const CARDS: Card[] = [
  card("1", "new", { children: [ref("2"), ref("3")] }),
  card("4", "new", { blocks: [ref("2")] }),
  card("2", "ready", { parent: ref("1"), blocked_by: [ref("4")] }),
  card("3", "ready", { parent: ref("1") }),
  card("5", "done"),
];

type Routes = Parameters<typeof fakeApi>[0];

function routes(cards: { open: Card[]; all?: Card[] } = { open: CARDS }, extra: Routes = {}): Routes {
  return {
    "GET /api/v1/session": json(200, SESSION),
    "GET /api/v1/workspaces": json(200, { items: [WORKSPACE], next_cursor: null }),
    "GET /api/v1/workspaces/acme": json(200, WORKSPACE),
    "GET /api/v1/members": json(200, MEMBERS),
    "GET /api/v1/admin/diagnostics": json(200, {}),
    "GET /api/v1/repos": json(200, { items: [], next_cursor: null }),
    "GET /api/v1/workspaces/acme/board/cards": (req) =>
      json(200, { items: req.search.includes("state=all") ? (cards.all ?? cards.open) : cards.open, next_cursor: null }),
    ...extra,
  };
}

const article = (key: string) => screen.getByRole("article", { name: new RegExp(`^acme/demo#${key} `) });
const anchor = (key: string) => article(key).closest("[data-ticket]") as HTMLElement;
/** What is dragged: the card's own box (an open parent's frame holds its children too). */
const draggable = (key: string) => article(key).closest(".rel-card") as HTMLElement;
/** A line behind the cards (with its "×" when solid), and its copy in front of them. */
const edge = (from: string, to: string) => document.querySelector<SVGGElement>(`.rel-lines g[data-from="acme/demo#${from}"][data-to="acme/demo#${to}"]`);
const inFront = (from: string, to: string) => document.querySelector(`.rel-front g[data-from="acme/demo#${from}"][data-to="acme/demo#${to}"]`);
const dataTransfer = () => ({ setData: () => {}, getData: () => "", effectAllowed: "" });

async function shown() {
  renderApp("/acme/-/relations");
  return screen.findByRole("article", { name: /^acme\/demo#1 / });
}

/** Opens #1, showing #2 and #3 in its frame. */
async function openParent(user: ReturnType<typeof userEvent.setup>) {
  await user.click(within(article("1")).getByRole("button", { expanded: false }));
  await screen.findByRole("article", { name: /^acme\/demo#2 / });
}

describe("relations", () => {
  it("is reached from the sidebar and lays out parents, their collapsed dependencies, and tickets without relations", async () => {
    const user = userEvent.setup();
    const fake = fakeApi(routes());
    renderApp("/acme");
    await user.click(await screen.findByRole("link", { name: "Relations" }));
    await screen.findByRole("article", { name: /^acme\/demo#1 / });
    expect(window.location.pathname).toBe("/acme/-/relations");
    expect(screen.getByRole("link", { name: "Relations" })).toHaveAttribute("aria-current", "page");

    // #2 and #3 are inside the collapsed #1; #4's dependency of #2 is drawn to #1, dotted.
    expect(screen.queryByRole("article", { name: /^acme\/demo#2 / })).not.toBeInTheDocument();
    expect(edge("4", "1")).toHaveClass("dashed");
    // A dotted line has no "×": which child's dependency it is, is not one.
    expect(within(edge("4", "1") as unknown as HTMLElement).queryByRole("button")).not.toBeInTheDocument();
    expect(fake.calls("GET", "/api/v1/workspaces/acme/board/cards").every((r) => r.search.includes("state=open") && r.search.includes("comments=none"))).toBe(true);

    // #5 has no relation: folded below the figure.
    expect(screen.queryByRole("article", { name: /^acme\/demo#5 / })).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: /Tickets without relations \(1\)/ }));
    expect(article("5")).toBeInTheDocument();
  });

  it("opens a parent into a frame with a toggle that says so, and remembers it", async () => {
    const user = userEvent.setup();
    fakeApi(routes());
    await shown();
    const toggle = within(article("1")).getByRole("button", { name: /Children 0\/2/ });
    expect(toggle).toHaveClass("card-control");
    expect(toggle).toHaveAttribute("aria-expanded", "false");
    expect(toggle).toHaveTextContent("▸");
    await user.hover(toggle);
    expect(await screen.findByText("Open child tickets")).toBeInTheDocument();

    await user.click(toggle);
    expect(await screen.findByRole("article", { name: /^acme\/demo#2 / })).toBeInTheDocument();
    // The same button, still focused: opening does not rebuild the card.
    expect(toggle).toHaveAttribute("aria-expanded", "true");
    expect(toggle).toHaveFocus();
    // The frame is #1's anchor; the line from #4 now reaches #2 itself, solid.
    expect(anchor("1")).toHaveClass("rel-frame");
    expect(anchor("2").parentElement?.closest("[data-ticket]")).toBe(anchor("1"));
    expect(edge("4", "2")).not.toHaveClass("dashed");
    expect(within(edge("4", "2") as unknown as HTMLElement).getByRole("button")).toBeInTheDocument();
    expect(edge("4", "1")).toBeNull();
    expect(JSON.parse(window.localStorage.getItem("branvil.relations.open.acme") ?? "[]")).toEqual(["acme/demo#1"]);
  });

  it("makes a dropped card a child, and undoes it", async () => {
    const fake = fakeApi(routes(undefined, { "PATCH /api/v1/repos/acme/demo/tickets/4": json(200, CARDS[1]) }));
    await shown();
    fireEvent.dragStart(draggable("4"), { dataTransfer: dataTransfer() });
    fireEvent.dragOver(article("1"));
    expect(anchor("1")).toHaveClass("drop-target");
    fireEvent.drop(article("1"));

    expect(await screen.findByText("Made acme/demo#4 a child of acme/demo#1.")).toBeInTheDocument();
    expect(fake.calls("PATCH", "/api/v1/repos/acme/demo/tickets/4")[0]?.body).toEqual({ parent: url("1") });
    await userEvent.setup().click(screen.getByRole("button", { name: "Undo" }));
    expect(await screen.findByText("Undone.")).toBeInTheDocument();
    expect(fake.calls("PATCH", "/api/v1/repos/acme/demo/tickets/4")[1]?.body).toEqual({ parent: null });
    expect(screen.queryByRole("button", { name: "Undo" })).not.toBeInTheDocument();
  });

  it("offers no drop on the dragged card itself or its current parent", async () => {
    const user = userEvent.setup();
    const fake = fakeApi(routes());
    await shown();
    await openParent(user);
    fireEvent.dragStart(draggable("2"), { dataTransfer: dataTransfer() });
    fireEvent.dragOver(anchor("1"));
    expect(anchor("1")).not.toHaveClass("drop-target");
    fireEvent.drop(anchor("1"));
    fireEvent.dragStart(draggable("2"), { dataTransfer: dataTransfer() });
    fireEvent.drop(article("2"));
    // A write would be sent after a tick (TanStack runs onMutate first).
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(fake.requests.some((r) => r.method === "PATCH")).toBe(false);
  });

  it("takes a child out of its parent when it is dropped on the background", async () => {
    const user = userEvent.setup();
    const fake = fakeApi(routes(undefined, { "PATCH /api/v1/repos/acme/demo/tickets/2": json(200, CARDS[2]) }));
    await shown();
    await openParent(user);
    fireEvent.dragStart(draggable("2"), { dataTransfer: dataTransfer() });
    const background = document.querySelector(".relations-canvas") as HTMLElement;
    fireEvent.dragOver(background);
    expect(background).toHaveClass("drop-target");
    fireEvent.drop(background);
    expect(await screen.findByText("Took acme/demo#2 out of acme/demo#1.")).toBeInTheDocument();
    expect(fake.calls("PATCH", "/api/v1/repos/acme/demo/tickets/2")[0]?.body).toEqual({ parent: null });
  });

  it("draws a dependency from a card's handle: the card it is dropped on waits for it", async () => {
    const fake = fakeApi(routes(undefined, { "PATCH /api/v1/repos/acme/demo/tickets/1": json(200, CARDS[0]) }));
    await shown();
    fireEvent.pointerDown(within(article("4")).getByTitle("Drag to another card: it waits for acme/demo#4"), { button: 0 });
    fireEvent.pointerMove(article("1"));
    expect(await screen.findByText("acme/demo#1 waits for acme/demo#4")).toBeInTheDocument();
    fireEvent.pointerUp(article("1"));

    expect(await screen.findByText("acme/demo#1 now waits for acme/demo#4.")).toBeInTheDocument();
    expect(fake.calls("PATCH", "/api/v1/repos/acme/demo/tickets/1")[0]?.body).toEqual({ add_blocked_by: [url("4")] });
    expect(screen.queryByText("acme/demo#1 waits for acme/demo#4")).not.toBeInTheDocument();
  });

  it("makes an open parent's card the header of its frame, where its lines start", async () => {
    const user = userEvent.setup();
    // #1 has the child #2 and blocks #9.
    fakeApi(
      routes({
        open: [
          card("1", "new", { children: [ref("2")], blocks: [ref("9")] }),
          card("2", "new", { parent: ref("1") }),
          card("9", "ready", { blocked_by: [ref("1")] }),
        ],
      }),
    );
    // jsdom lays nothing out: the frame is 600×400, its header 600×80 at its top, #9 to the right.
    const box = (left: number, top: number, width: number, height: number) => DOMRect.fromRect({ x: left, y: top, width, height });
    vi.spyOn(Element.prototype, "getBoundingClientRect").mockImplementation(function (this: Element) {
      const ref = this.closest("[data-ticket]")?.getAttribute("data-ticket");
      if (ref === "acme/demo#1") return this.matches(".rel-frame") ? box(0, 0, 600, 400) : box(0, 0, 600, 80);
      if (ref === "acme/demo#9") return box(700, 100, 260, 80);
      return box(0, 0, 0, 0);
    });
    await shown();
    await openParent(user);
    expect(article("1").closest(".rel-card")?.parentElement).toBe(anchor("1"));
    expect(anchor("1").firstElementChild).toBe(article("1").closest(".rel-card"));
    // From the header's right edge, at its middle; not from the middle of the frame.
    await waitFor(() => expect(edge("1", "9")?.querySelector("path")?.getAttribute("d")).toMatch(/^M 600 40 /));
  });

  it("offers no line onto the card's own frame or a card inside it", async () => {
    const user = userEvent.setup();
    const fake = fakeApi(routes());
    await shown();
    await openParent(user);
    fireEvent.pointerDown(within(article("1")).getByTitle("Drag to another card: it waits for acme/demo#1"), { button: 0 });
    fireEvent.pointerMove(article("3"));
    expect(screen.queryByText(/waits for acme\/demo#1$/)).not.toBeInTheDocument();
    fireEvent.pointerUp(article("3"));
    fireEvent.pointerDown(within(article("2")).getByTitle("Drag to another card: it waits for acme/demo#2"), { button: 0 });
    fireEvent.pointerUp(anchor("1"));
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(fake.requests.some((r) => r.method === "PATCH")).toBe(false);
  });

  it("draws the lines behind the cards and a hovered or focused one again in front of them", async () => {
    const user = userEvent.setup();
    fakeApi(routes());
    await shown();
    await openParent(user);
    expect(inFront("4", "2")).not.toHaveClass("front");
    await user.hover(edge("4", "2") as SVGGElement);
    expect(inFront("4", "2")).toHaveClass("front");
    await user.unhover(edge("4", "2") as SVGGElement);
    expect(inFront("4", "2")).not.toHaveClass("front");
    // The keyboard reaches a line through its "×".
    within(edge("4", "2") as unknown as HTMLElement).getByRole("button").focus();
    await waitFor(() => expect(inFront("4", "2")).toHaveClass("front"));
  });

  it("removes a dependency with the line's ×, and undoes it", async () => {
    const user = userEvent.setup();
    const fake = fakeApi(routes(undefined, { "PATCH /api/v1/repos/acme/demo/tickets/2": json(200, CARDS[2]) }));
    await shown();
    await openParent(user);
    await user.click(screen.getByRole("button", { name: "Remove the dependency: acme/demo#2 waits for acme/demo#4" }));
    expect(await screen.findByText("acme/demo#2 no longer waits for acme/demo#4.")).toBeInTheDocument();
    expect(fake.calls("PATCH", "/api/v1/repos/acme/demo/tickets/2")[0]?.body).toEqual({ remove_blocked_by: [url("4")] });
    await user.click(screen.getByRole("button", { name: "Undo" }));
    await waitFor(() => expect(fake.calls("PATCH", "/api/v1/repos/acme/demo/tickets/2")[1]?.body).toEqual({ add_blocked_by: [url("4")] }));
  });

  it("shows the service's refusal as the ticket page does and leaves the figure as it was", async () => {
    const fake = fakeApi(
      routes(undefined, { "PATCH /api/v1/repos/acme/demo/tickets/1": apiError(400, "invalid", "Tickets would wait on each other forever.") }),
    );
    await shown();
    fireEvent.dragStart(draggable("1"), { dataTransfer: dataTransfer() });
    fireEvent.drop(article("4"));
    expect(await screen.findByRole("alert")).toHaveTextContent("Tickets would wait on each other forever.");
    expect(fake.calls("PATCH", "/api/v1/repos/acme/demo/tickets/1")).toHaveLength(1);
    expect(edge("4", "1")).toHaveClass("dashed");
    expect(screen.queryByRole("button", { name: "Undo" })).not.toBeInTheDocument();
  });

  it("changes a card's lane and assignee in place, with the board's refusal", async () => {
    const user = userEvent.setup();
    const fake = fakeApi(
      routes(undefined, {
        "PUT /api/v1/repos/acme/demo/tickets/4/board": [apiError(409, "blocked", "blocked"), json(200, CARDS[1])],
        "PUT /api/v1/repos/acme/demo/tickets/4/assignee": json(200, CARDS[1]),
      }),
    );
    await shown();
    const lane = within(article("4")).getByRole("button", { name: "Lane of acme/demo#4: New — click to change" });
    expect(lane).toHaveClass("card-control");
    await pickFromMenu(user, lane.getAttribute("aria-label") as string, "In progress", article("4"));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Could not move acme/demo#4; it is back where it was. It can be moved to In progress once every ticket it is blocked by is closed.",
    );
    await pickFromMenu(user, "Lane of acme/demo#4: New — click to change", "Ready", article("4"));
    expect(await screen.findByText("acme/demo#4 moved to Ready.")).toBeInTheDocument();
    expect(fake.calls("PUT", "/api/v1/repos/acme/demo/tickets/4/board").map((r) => r.body)).toEqual([{ lane: "in_progress" }, { lane: "ready" }]);

    // The lane it is in is no move.
    await pickFromMenu(user, "Lane of acme/demo#4: New — click to change", "New", article("4"));
    expect(fake.calls("PUT", "/api/v1/repos/acme/demo/tickets/4/board")).toHaveLength(2);

    await pickFromMenu(user, /^Assignee of acme\/demo#4: /, "aiko", article("4"));
    await waitFor(() => expect(fake.calls("PUT", "/api/v1/repos/acme/demo/tickets/4/assignee")[0]?.body).toEqual({ name: "aiko" }));
  });

  it("shows the cards the filters do not match faintly instead of hiding them", async () => {
    const user = userEvent.setup();
    fakeApi(routes());
    await shown();
    await user.type(screen.getByRole("textbox", { name: "Text" }), "Title of 4");
    await waitFor(() => expect(article("1")).toHaveClass("dim"));
    expect(article("4")).not.toHaveClass("dim");
  });

  it("reads closed tickets for a closed parent of an open one, and hides the others until asked", async () => {
    const user = userEvent.setup();
    // #7 is closed with an open child #8; #9 is closed and blocks #8.
    const open = [card("8", "ready", { parent: ref("7", "closed"), blocked_by: [ref("9", "closed")] })];
    const all = [
      card("7", "done", { state: "closed", children: [ref("8")] }),
      card("9", "done", { state: "closed", blocks: [ref("8")] }),
      ...open,
    ];
    const fake = fakeApi(routes({ open, all }));
    renderApp("/acme/-/relations");
    expect(await screen.findByRole("article", { name: /^acme\/demo#7 / })).toBeInTheDocument();
    expect(fake.calls("GET", "/api/v1/workspaces/acme/board/cards").some((r) => r.search.includes("state=all"))).toBe(true);
    expect(screen.queryByRole("article", { name: /^acme\/demo#9 / })).not.toBeInTheDocument();
    expect(edge("9", "7")).toBeNull();

    await user.click(screen.getByRole("checkbox", { name: "Include closed tickets" }));
    expect(await screen.findByRole("article", { name: /^acme\/demo#9 / })).toBeInTheDocument();
    expect(window.localStorage.getItem("branvil.relations.includeClosed")).toBe("true");
    expect(window.localStorage.getItem("branvil.board.includeClosed")).toBeNull();
  });
});
