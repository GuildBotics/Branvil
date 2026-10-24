// Pure layout logic of the relations graph (unit tested).

import { ticketRef, type Card, type Lane } from "../../api/types";

// Cards are identified by their ticketRef and arrive in board order (lane order, then rank).
// The figure puts the lanes that matter most first and keeps the board's order
// within a lane: inside a column, among children, and in the frame of new tickets.
const PRIORITY: readonly Lane[] = ["in_progress", "ready", "new", "done"];

export interface GraphNode {
  card: Card;
  ref: string;
  /** Whether the graph shows children of it (whether it has an open/close toggle). */
  hasChildren: boolean;
  /** The level of its children while it is open; null while closed or without children. */
  inner: Level | null;
}

/** The new tickets without relations, in one frame laid out where the first of them would be. */
export interface NewFrame {
  fresh: Card[];
}

/** Columns from left to right; each column in board order. */
export type Level = (GraphNode | NewFrame)[][];

export interface GraphEdge {
  /** The ref of the shown node the line starts from (the blocker side). */
  from: string;
  /** The ref of the shown node the line ends at (the blocked side). */
  to: string;
  /** Whether an end is a descendant gathered into a closed parent. */
  dashed: boolean;
  /** Solid lines only: the dependency itself. */
  blocker?: Card;
  blocked?: Card;
}

export interface Graph {
  level: Level;
  edges: GraphEdge[];
  /** The tickets with a parent, a child, or a dependency among the shown ones. */
  related: ReadonlySet<string>;
}

export function buildGraph(board: Card[], options: { includeClosed: boolean; open: ReadonlySet<string> }): Graph {
  const cards = board.toSorted((a, b) => PRIORITY.indexOf(a.lane) - PRIORITY.indexOf(b.lane));
  const loaded = new Map(cards.map((c) => [ticketRef(c), c]));
  // A closed ticket with an open descendant stays shown.
  const kept = new Set<string>();
  for (const c of cards) {
    if (c.state !== "open") continue;
    for (let p = c.parent; p && !kept.has(ticketRef(p)); p = loaded.get(ticketRef(p))?.parent ?? null) kept.add(ticketRef(p));
  }
  const shown = cards.filter((c) => options.includeClosed || c.state === "open" || kept.has(ticketRef(c)));
  const byRef = new Map(shown.map((c) => [ticketRef(c), c]));
  const parentOf = (c: Card) => (c.parent ? byRef.get(ticketRef(c.parent)) : undefined);

  const children = new Map<string, Card[]>();
  for (const c of shown) {
    const p = parentOf(c);
    if (p) append(children, ticketRef(p), c);
  }
  /** The refs from the root down to the card. */
  const path = (c: Card): string[] => {
    const p = parentOf(c);
    return [...(p ? path(p) : []), ticketRef(c)];
  };
  /** The card itself, or its topmost closed ancestor. */
  const rep = (c: Card) => path(c).slice(0, -1).find((a) => !options.open.has(a)) ?? ticketRef(c);

  const related = new Set<string>([...children.keys()].concat(shown.filter(parentOf).map(ticketRef)));
  // Within a level, sibling a precedes sibling b when a's subtree blocks b's subtree.
  const preds = new Map<string, string[]>();
  const edges = new Map<string, GraphEdge>();
  for (const blocked of shown) {
    for (const r of blocked.blocked_by) {
      const blocker = byRef.get(ticketRef(r));
      if (!blocker) continue;
      related.add(ticketRef(blocker)).add(ticketRef(blocked));
      const [px, py] = [path(blocker), path(blocked)];
      const i = px.findIndex((a, j) => a !== py[j]);
      const [a, b] = [px[i], py[i]];
      // No line when one end is an ancestor of the other: it would run from inside a frame to the frame.
      if (i === -1 || a === undefined || b === undefined) continue;
      append(preds, b, a);
      const [from, to] = [rep(blocker), rep(blocked)];
      if (from === to) continue;
      const dashed = from !== ticketRef(blocker) || to !== ticketRef(blocked);
      const key = `${from}>${to}`;
      if (dashed && !edges.has(key)) edges.set(key, { from, to, dashed });
      if (!dashed) edges.set(key, { from, to, dashed, blocker, blocked });
    }
  }

  // Column = the longest chain of predecessors; an edge back to a node in progress (a cycle) is ignored.
  const column = new Map<string, number>();
  const visiting = new Set<string>();
  const columnOf = (ref: string): number => {
    const known = column.get(ref);
    if (known !== undefined) return known;
    visiting.add(ref);
    let c = 0;
    for (const p of preds.get(ref) ?? []) if (!visiting.has(p)) c = Math.max(c, columnOf(p) + 1);
    visiting.delete(ref);
    column.set(ref, c);
    return c;
  };
  for (const c of shown) columnOf(ticketRef(c));

  const isFresh = (c: Card) => c.lane === "new" && !related.has(ticketRef(c));
  const fresh = shown.filter(isFresh);
  const levelOf = (members: Card[]): Level => {
    const level: Level = [];
    for (const card of members) {
      const ref = ticketRef(card);
      const kids = children.get(ref);
      const item = isFresh(card)
        ? card === fresh[0] && { fresh }
        : { card, ref, hasChildren: !!kids, inner: kids && options.open.has(ref) ? levelOf(kids) : null };
      if (item) (level[columnOf(ref)] ??= []).push(item);
    }
    return level;
  };

  return { level: levelOf(shown.filter((c) => !parentOf(c))), edges: [...edges.values()], related };
}

function append<T>(map: Map<string, T[]>, key: string, value: T) {
  const list = map.get(key);
  if (list) list.push(value);
  else map.set(key, [value]);
}
