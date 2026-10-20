// The relations figure: the levels of cards and frames laid out by CSS, the
// lines between them drawn over it in SVG, and the drags that relate tickets.

import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, type DragEvent, type PointerEvent, type RefObject } from "react";
import { ticketRef, type Card, type Lane } from "../../api/types";
import type { Person } from "../../components/PeoplePicker";
import { useT } from "../../i18n";
import { matchesFilter, type CardFilter } from "../board/ordering";
import { linkEdit, parentEdit, unlinkEdit, type Edit } from "./edits";
import type { Graph, GraphEdge, Level } from "./graph";
import { RelationCard } from "./RelationCard";

/** The drop target meaning "the figure's background": out of every parent. */
const BACKGROUND = "\u0000background";

/** What the figure is drawn with and reports back. */
interface Props {
  graph: Graph;
  cards: Card[];
  lanes: Lane[];
  people: Person[];
  loadChecks: Set<string>;
  filter: CardFilter;
  open: ReadonlySet<string>;
  /** The ticket whose lane is being changed. */
  moving: string | null;
  /** Whether relations may be changed (false while the tickets shown are incomplete or a change is being written). */
  editable: boolean;
  onToggle: (ref: string) => void;
  onLane: (card: Card, lane: Lane) => void;
  onEdit: (edit: Edit) => void;
}

/** The ticket an event happened over: its anchor's ref, or null. */
const refUnder = (target: EventTarget | null) => (target instanceof Element ? (target.closest("[data-ticket]")?.getAttribute("data-ticket") ?? null) : null);

const edgeKey = (e: GraphEdge) => `${e.from}>${e.to}`;

export function Figure({ graph, cards, lanes, people, loadChecks, filter, open, moving, editable, onToggle, onLane, onEdit }: Props) {
  const t = useT();
  const scrollRef = useRef<HTMLDivElement>(null);
  const contentRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLDivElement>(null);
  const linkingRef = useRef(false);
  const startLinkRef = useRef<(card: Card, e: PointerEvent<HTMLElement>) => void>(() => {});
  const [dragging, setDragging] = useState<Card | null>(null);
  const [target, setTarget] = useState<string | null>(null);
  const [unrelatedOpen, setUnrelatedOpen] = useState(false);
  const byRef = useMemo(() => new Map(cards.map((c) => [ticketRef(c), c])), [cards]);

  const onLinkStart = useCallback((card: Card, e: PointerEvent<HTMLElement>) => startLinkRef.current(card, e), []);

  // Lines follow the cards: redrawn once the layout is done and whenever a card or the figure changes size.
  useLayoutEffect(() => {
    const scroll = scrollRef.current;
    const content = contentRef.current;
    if (!scroll || !content) return;
    const draw = () => {
      const origin = content.getBoundingClientRect();
      const rects = new Map<string, DOMRect>();
      // A line meets a ticket's own card: an open parent's is its frame's header, as wide as the frame.
      for (const el of content.querySelectorAll<HTMLElement>("[data-ticket]"))
        rects.set(el.dataset["ticket"] as string, (el.querySelector(":scope > .rel-card") ?? el).getBoundingClientRect());
      for (const g of content.querySelectorAll<SVGGElement>("g[data-from]")) {
        const a = rects.get(g.dataset["from"] as string);
        const b = rects.get(g.dataset["to"] as string);
        if (!a || !b) continue;
        const [x1, y1] = [a.right - origin.left, a.top + a.height / 2 - origin.top];
        const [x2, y2] = [b.left - origin.left, b.top + b.height / 2 - origin.top];
        const d = curve(x1, y1, x2, y2);
        for (const p of g.querySelectorAll("path")) p.setAttribute("d", d);
        const remove = g.querySelector("foreignObject");
        remove?.setAttribute("x", String(x2 - 38));
        remove?.setAttribute("y", String(y2 - 12));
      }
      // The tickets without relations stay in view as the figure scrolls sideways.
      content.style.setProperty("--view-width", `${scroll.clientWidth}px`);
    };
    draw();
    const observer = new ResizeObserver(draw);
    observer.observe(scroll);
    observer.observe(content);
    for (const el of content.querySelectorAll("[data-ticket]")) observer.observe(el);
    return () => observer.disconnect();
  }, [graph, unrelatedOpen]);

  // A drop target is a card or frame (`data-ticket`) or, inside the figure, its background;
  // it is offered only when dropping there changes something.
  const editFor = (dragged: Card, e: DragEvent): [string | null, Edit | null] => {
    const ref = refUnder(e.target) ?? (e.target instanceof Node && canvasRef.current?.contains(e.target) ? BACKGROUND : null);
    if (ref === null) return [null, null];
    const over = ref === BACKGROUND ? null : byRef.get(ref);
    if (over === undefined) return [null, null];
    return [ref, parentEdit(dragged, over)];
  };
  const dragProps = (card: Card) => ({
    draggable: editable,
    onDragStart: (e: DragEvent) => {
      e.stopPropagation();
      if (linkingRef.current) {
        e.preventDefault();
        return;
      }
      e.dataTransfer.setData("text/plain", ticketRef(card));
      e.dataTransfer.effectAllowed = "move";
      setDragging(card);
    },
    onDragEnd: () => {
      setDragging(null);
      setTarget(null);
    },
  });
  const onDragOver = (e: DragEvent) => {
    if (!dragging) return;
    const [ref, edit] = editFor(dragging, e);
    if (edit) e.preventDefault();
    const next = edit ? ref : null;
    setTarget((prev) => (prev === next ? prev : next));
  };
  const onDrop = (e: DragEvent) => {
    if (!dragging) return;
    e.preventDefault();
    const [, edit] = editFor(dragging, e);
    setDragging(null);
    setTarget(null);
    if (edit) onEdit(edit);
  };

  const node = (card: Card, isOpen: boolean | undefined) => (
    <RelationCard
      busy={moving === ticketRef(card)}
      card={card}
      lanes={lanes}
      people={people}
      loadChecks={loadChecks}
      open={isOpen}
      dim={!matchesFilter(card, filter)}
      onToggle={onToggle}
      onLane={onLane}
      onLinkStart={onLinkStart}
    />
  );
  const anchorClass = (base: string, ref: string) => `${base}${target === ref ? " drop-target" : ""}${dragging && ticketRef(dragging) === ref ? " dragging" : ""}`;
  // An anchor is a card, or an open parent's frame: its card as the header, then its children's level.
  // Opening keeps the card where it was in the tree, so it keeps the focus.
  const anchor = (card: Card, inner: Level | null, isOpen: boolean | undefined) => {
    const ref = ticketRef(card);
    return (
      <div key={ref} className={anchorClass(inner ? "rel-frame" : "rel-node", ref)} data-ticket={ref}>
        <div className="rel-card" {...dragProps(card)}>
          {node(card, isOpen)}
        </div>
        {inner && level(inner)}
      </div>
    );
  };
  const level = (l: Level) => (
    <div className="rel-level">
      {l.map((column, i) => (
        <div className="rel-column" key={i}>
          {column.map((n) => anchor(n.card, n.inner, n.hasChildren ? open.has(n.ref) : undefined))}
        </div>
      ))}
    </div>
  );

  return (
    <div className="relations-scroll" ref={scrollRef}>
      <div
        ref={contentRef}
        className={`relations-content${dragging ? " relating" : ""}`}
        onDragOver={onDragOver}
        onDrop={onDrop}
      >
        <div ref={canvasRef} className={`relations-canvas${target === BACKGROUND ? " drop-target" : ""}`}>
          {graph.level.length === 0 ? <p className="relations-empty">{t("relations.empty")}</p> : level(graph.level)}
        </div>
        {graph.unrelated.length > 0 && (
          <section className="relations-unrelated" aria-label={t("relations.unrelated", { count: graph.unrelated.length })}>
            <button type="button" className="card-control" aria-expanded={unrelatedOpen} onClick={() => setUnrelatedOpen(!unrelatedOpen)}>
              {unrelatedOpen ? "▾" : "▸"} {t("relations.unrelated", { count: graph.unrelated.length })}
            </button>
            {unrelatedOpen && (
              <div className="relations-tiles">
                {graph.unrelated.map((c) => anchor(c, null, undefined))}
              </div>
            )}
          </section>
        )}
        <Lines edges={graph.edges} editable={editable} onEdit={onEdit} />
        <LinkDraft contentRef={contentRef} startRef={startLinkRef} linkingRef={linkingRef} byRef={byRef} editable={editable} onEdit={onEdit} />
      </div>
    </div>
  );
}

/** A horizontal S-curve from the right of one box to the left of another. */
function curve(x1: number, y1: number, x2: number, y2: number): string {
  const dx = Math.max(40, Math.abs(x2 - x1) / 2);
  return `M ${x1} ${y1} C ${x1 + dx} ${y1}, ${x2 - dx} ${y2}, ${x2} ${y2}`;
}

/** The arrowhead of a layer's lines (each layer colours its own). */
const arrow = (id: string) => (
  <defs>
    <marker id={id} viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse">
      <path d="M 0 0 L 10 5 L 0 10 z" />
    </marker>
  </defs>
);

/**
 * The lines, behind the cards so the cards stay readable: they show in the gaps
 * between them. A line hovered there (or whose "×" has the focus) is drawn
 * again in front of the cards, so it can be followed across them. Its own
 * state, so a hover re-renders only the lines.
 */
function Lines({ edges, editable, onEdit }: { edges: GraphEdge[]; editable: boolean; onEdit: (edit: Edit) => void }) {
  const t = useT();
  const [front, setFront] = useState<string | null>(null);
  return (
    <>
      <svg className="rel-lines" role="group" aria-label={t("relations.title")}>
        {arrow("rel-arrow")}
        {edges.map((e) => {
          const key = edgeKey(e);
          const show = () => setFront(key);
          const hide = () => setFront((k) => (k === key ? null : k));
          return (
            <g
              key={key}
              className={`rel-edge${e.dashed ? " dashed" : ""}`}
              data-from={e.from}
              data-to={e.to}
              onPointerEnter={show}
              onPointerLeave={hide}
              onFocus={show}
              onBlur={hide}
            >
              <path className="rel-edge-line" markerEnd="url(#rel-arrow)" />
              <path className="rel-edge-hit" />
              {e.blocker && e.blocked && (
                // Just before the arrow, in the gap left of its card: never under a card.
                <foreignObject width="24" height="24">
                  <button
                    type="button"
                    className="rel-edge-remove"
                    aria-label={t("relations.unlink", { blocked: e.to, blocker: e.from })}
                    title={t("relations.unlink", { blocked: e.to, blocker: e.from })}
                    disabled={!editable}
                    onClick={() => onEdit(unlinkEdit(e.blocker as Card, e.blocked as Card))}
                  >
                    ×
                  </button>
                </foreignObject>
              )}
            </g>
          );
        })}
      </svg>
      <svg className="rel-front" aria-hidden>
        {arrow("rel-arrow-front")}
        {edges.map((e) => (
          <g key={edgeKey(e)} className={`rel-edge${e.dashed ? " dashed" : ""}${front === edgeKey(e) ? " front" : ""}`} data-from={e.from} data-to={e.to}>
            <path className="rel-edge-line" markerEnd="url(#rel-arrow-front)" />
          </g>
        ))}
      </svg>
    </>
  );
}

interface Draft {
  from: Card;
  x1: number;
  y1: number;
  x2: number;
  y2: number;
  /** The card under the pointer that the line can be dropped on. */
  to: Card | null;
}

/**
 * The line being drawn from a card's handle to the pointer. Its state is its
 * own, so following the pointer re-renders only it; the figure calls it through `startRef`.
 */
function LinkDraft({
  contentRef,
  startRef,
  linkingRef,
  byRef,
  editable,
  onEdit,
}: {
  contentRef: RefObject<HTMLDivElement | null>;
  startRef: RefObject<(card: Card, e: PointerEvent<HTMLElement>) => void>;
  linkingRef: RefObject<boolean>;
  byRef: Map<string, Card>;
  editable: boolean;
  onEdit: (edit: Edit) => void;
}) {
  const t = useT();
  const [draft, setDraft] = useState<Draft | null>(null);
  const stopRef = useRef<() => void>(() => {});

  useLayoutEffect(() => {
    startRef.current = (from, e) => {
      const content = contentRef.current;
      if (!editable || !content || e.button !== 0) return;
      const at = (x: number, y: number) => {
        const origin = content.getBoundingClientRect();
        return [x - origin.left, y - origin.top] as const;
      };
      const handle = e.currentTarget.getBoundingClientRect();
      const [x1, y1] = at(handle.left + handle.width / 2, handle.top + handle.height / 2);
      const origin = e.currentTarget.closest("[data-ticket]");
      // Not its own frame or a card in its frame: a dependency between a parent and its descendant is not drawn.
      const toUnder = (target: EventTarget | null) => {
        const el = target instanceof Element ? target.closest("[data-ticket]") : null;
        if (!el || !origin || el.contains(origin) || origin.contains(el)) return null;
        const over = byRef.get(el.getAttribute("data-ticket") ?? "");
        return over && linkEdit(from, over) ? over : null;
      };
      const move = (ev: globalThis.PointerEvent) => {
        const [x2, y2] = at(ev.clientX, ev.clientY);
        setDraft({ from, x1, y1, x2, y2, to: toUnder(ev.target) });
      };
      const up = (ev: globalThis.PointerEvent) => {
        const to = toUnder(ev.target);
        stopRef.current();
        const edit = to && linkEdit(from, to);
        if (edit) onEdit(edit);
      };
      stopRef.current = () => {
        window.removeEventListener("pointermove", move);
        window.removeEventListener("pointerup", up);
        window.removeEventListener("pointercancel", stopRef.current);
        content.classList.remove("relating");
        linkingRef.current = false;
        setDraft(null);
      };
      window.addEventListener("pointermove", move);
      window.addEventListener("pointerup", up);
      window.addEventListener("pointercancel", stopRef.current);
      // Lines under the pointer would hide the cards from it.
      content.classList.add("relating");
      linkingRef.current = true;
      setDraft({ from, x1, y1, x2: x1, y2: y1, to: null });
    };
  }, [startRef, contentRef, linkingRef, byRef, editable, onEdit]);
  useEffect(() => () => stopRef.current(), []);

  if (!draft) return null;
  return (
    <>
      <svg className="rel-draft" aria-hidden>
        {arrow("rel-arrow-draft")}
        <path d={curve(draft.x1, draft.y1, draft.x2, draft.y2)} markerEnd="url(#rel-arrow-draft)" />
      </svg>
      {draft.to && (
        <div className="rel-draft-hint" role="status" style={{ left: draft.x2 + 14, top: draft.y2 + 14 }}>
          {t("relations.waits", { blocked: ticketRef(draft.to), blocker: ticketRef(draft.from) })}
        </div>
      )}
    </>
  );
}
