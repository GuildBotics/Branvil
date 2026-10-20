import { memo, type PointerEvent } from "react";
import { ticketRef, type Card, type Lane } from "../../api/types";
import type { Person } from "../../components/PeoplePicker";
import { useT } from "../../i18n";
import { CardView, ChildrenToggle, LaneButton } from "../board/CardView";

/**
 * A card of the relations page: the board's card with its lane and assignee
 * changed in place, its children opened or closed, and a handle that draws a
 * dependency. Memoised: the page passes stable props, so a drag over the
 * figure does not re-render it.
 */
export const RelationCard = memo(function RelationCard({
  card,
  lanes,
  people,
  loadChecks,
  open,
  dim,
  busy,
  onToggle,
  onLane,
  onLinkStart,
}: {
  card: Card;
  lanes: Lane[];
  people: Person[];
  loadChecks: Set<string>;
  /** Whether its children are shown; undefined for a card without children in the figure. */
  open: boolean | undefined;
  /** Not matching the filters: shown faintly, so the lines can still be followed. */
  dim: boolean;
  /** Its lane is being changed. */
  busy: boolean;
  onToggle: (ref: string) => void;
  onLane: (card: Card, lane: Lane) => void;
  onLinkStart: (card: Card, e: PointerEvent<HTMLElement>) => void;
}) {
  const t = useT();
  return (
    <CardView
      card={card}
      people={people}
      loadChecks={loadChecks}
      busy={busy}
      // A parent has the colour of the header it becomes once opened.
      className={[open !== undefined && "parent", dim && "dim"].filter(Boolean).join(" ")}
      lane={<LaneButton card={card} lanes={lanes} onLane={onLane} disabled={busy} />}
      lead={open === undefined ? undefined : <ChildrenToggle card={card} open={open} onToggle={onToggle} />}
    >
      <span
        className="rel-handle"
        title={t("relations.linkHandle", { key: ticketRef(card) })}
        // Keeps the card's own (native) drag and text selection from starting.
        onMouseDown={(e) => e.preventDefault()}
        onPointerDown={(e) => {
          e.stopPropagation();
          onLinkStart(card, e);
        }}
      />
    </CardView>
  );
});
