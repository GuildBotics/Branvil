import { describe, expect, it } from "vitest";
import type { Card, TicketRef } from "../api/types";
import { linkEdit, parentEdit, unlinkEdit } from "../views/relations/edits";
import { card } from "./fixtures";

const ref = (c: Card): TicketRef => ({ url: c.url, repository: c.repository, key: c.key, title: c.title, state: c.state, state_reason: null });

const url = (key: string) => `${window.location.origin}/acme/demo/-/tickets/${key}`;

describe("relations edits", () => {
  const parent = card("1", "new");
  const other = card("2", "new");
  const orphan = card("3", "new");
  const child = card("4", "new", { parent: ref(parent) });

  it("makes the dragged card a child of the target, undone by taking it out again", () => {
    expect(parentEdit(orphan, other)).toEqual({
      change: { repo: "acme/demo", key: "3", patch: { parent: url("2") } },
      inverse: { repo: "acme/demo", key: "3", patch: { parent: null } },
      message: "relations.madeChild",
      params: { child: "acme/demo#3", parent: "acme/demo#2" },
    });
  });

  it("moves a child to another parent, undone by moving it back", () => {
    expect(parentEdit(child, other)).toEqual({
      change: { repo: "acme/demo", key: "4", patch: { parent: url("2") } },
      inverse: { repo: "acme/demo", key: "4", patch: { parent: url("1") } },
      message: "relations.madeChild",
      params: { child: "acme/demo#4", parent: "acme/demo#2" },
    });
  });

  it("takes a child out of its parent when dropped on the background", () => {
    expect(parentEdit(child, null)).toEqual({
      change: { repo: "acme/demo", key: "4", patch: { parent: null } },
      inverse: { repo: "acme/demo", key: "4", patch: { parent: url("1") } },
      message: "relations.unparented",
      params: { child: "acme/demo#4", parent: "acme/demo#1" },
    });
  });

  it("changes nothing on itself, on its current parent, or on the background without a parent", () => {
    expect(parentEdit(orphan, orphan)).toBeNull();
    expect(parentEdit(child, parent)).toBeNull();
    expect(parentEdit(orphan, null)).toBeNull();
    // The same number in another repository is another ticket.
    expect(parentEdit(child, card("1", "new", { repository: "acme/web" }))?.change.patch).toEqual({
      parent: `${window.location.origin}/acme/web/-/tickets/1`,
    });
  });

  it("makes the ticket a line is drawn to wait for the one it starts from", () => {
    expect(linkEdit(parent, other)).toEqual({
      change: { repo: "acme/demo", key: "2", patch: { add_blocked_by: [url("1")] } },
      inverse: { repo: "acme/demo", key: "2", patch: { remove_blocked_by: [url("1")] } },
      message: "relations.linked",
      params: { blocked: "acme/demo#2", blocker: "acme/demo#1" },
    });
  });

  it("draws no line onto itself or onto a ticket already waiting", () => {
    expect(linkEdit(parent, parent)).toBeNull();
    expect(linkEdit(parent, card("2", "new", { blocked_by: [ref(parent)] }))).toBeNull();
  });

  it("removes a dependency, undone by adding it back", () => {
    expect(unlinkEdit(parent, other)).toEqual({
      change: { repo: "acme/demo", key: "2", patch: { remove_blocked_by: [url("1")] } },
      inverse: { repo: "acme/demo", key: "2", patch: { add_blocked_by: [url("1")] } },
      message: "relations.unlinked",
      params: { blocked: "acme/demo#2", blocker: "acme/demo#1" },
    });
  });
});
