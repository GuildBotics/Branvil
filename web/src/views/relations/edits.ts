// The ticket updates the relations graph's drags make (unit tested).

import type { TicketPatch } from "../../api/endpoints";
import { ticketRef, type Card } from "../../api/types";
import type { MessageKey } from "../../i18n/en";

export interface Change {
  repo: string;
  key: string;
  patch: TicketPatch;
}

export type EditMessage = Extract<
  MessageKey,
  "relations.madeChild" | "relations.unparented" | "relations.linked" | "relations.unlinked"
>;

export interface Edit {
  change: Change;
  /** The change that writes the relation back as it was. */
  inverse: Change;
  message: EditMessage;
  params: Record<string, string>;
}

function edit(card: Card, patch: TicketPatch, inverse: TicketPatch, message: EditMessage, params: Record<string, string>): Edit {
  const change = (p: TicketPatch) => ({ repo: card.repository, key: card.key, patch: p });
  return { change: change(patch), inverse: change(inverse), message, params };
}

/**
 * The update for dropping a card onto `target` (null: the graph's background);
 * null when it changes nothing. Cycles are left to the server to refuse.
 */
export function parentEdit(dragged: Card, target: Card | null): Edit | null {
  const parent = dragged.parent;
  const child = ticketRef(dragged);
  if (!target) {
    if (!parent) return null;
    return edit(dragged, { parent: null }, { parent: parent.url }, "relations.unparented", { child, parent: ticketRef(parent) });
  }
  if (ticketRef(target) === child || (parent && ticketRef(target) === ticketRef(parent))) return null;
  return edit(dragged, { parent: target.url }, { parent: parent?.url ?? null }, "relations.madeChild", {
    child,
    parent: ticketRef(target),
  });
}

/** The update for drawing a line from `blocker` to `blocked`; null when it changes nothing. */
export function linkEdit(blocker: Card, blocked: Card): Edit | null {
  const ref = ticketRef(blocker);
  if (ref === ticketRef(blocked) || blocked.blocked_by.some((r) => ticketRef(r) === ref)) return null;
  return dependency(blocker, blocked, true);
}

/** The update for the × on a solid line. */
export function unlinkEdit(blocker: Card, blocked: Card): Edit {
  return dependency(blocker, blocked, false);
}

function dependency(blocker: Card, blocked: Card, add: boolean): Edit {
  const on = { add_blocked_by: [blocker.url] };
  const off = { remove_blocked_by: [blocker.url] };
  return edit(blocked, add ? on : off, add ? off : on, add ? "relations.linked" : "relations.unlinked", {
    blocked: ticketRef(blocked),
    blocker: ticketRef(blocker),
  });
}
