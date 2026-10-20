import { Button, Group, Text } from "@mantine/core";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useCallback, useMemo, useState } from "react";
import * as api from "../api/endpoints";
import { ticketRef, type Card, type Lane } from "../api/types";
import { ErrorMessage, Loading, Refusal } from "../components/common";
import { PageHeader } from "../components/PageHeader";
import type { Person } from "../components/PeoplePicker";
import { useT } from "../i18n";
import { useBoard, useCards, useMembers } from "../query/hooks";
import { useStored } from "../storage";
import { CardFilters } from "./board/CardFilters";
import { ciUrlsOf, moveRefusal, type Notice } from "./board/CardView";
import type { CardFilter } from "./board/ordering";
import type { Change, Edit } from "./relations/edits";
import { Figure } from "./relations/Figure";
import { buildGraph, type Graph, type Level } from "./relations/graph";

/** A relation written: what it changes, what the notice then says, and what undoes it. */
interface Write {
  change: Change;
  done: string;
  undo?: Change;
}

/** What the page says after a change: the board's notice with an undo, or a refusal of the service as the ticket page shows it. */
type Said = (Notice & { undo?: Change }) | { tone: "error"; error: unknown };

/** The parents a viewer opened, as kept in the browser (a JSON array of refs). */
function openParents(stored: string | null): Set<string> {
  try {
    const refs: unknown = JSON.parse(stored ?? "[]");
    return new Set(Array.isArray(refs) ? refs.filter((r): r is string => typeof r === "string") : []);
  } catch {
    return new Set();
  }
}

/** The cards a figure shows: those of its levels (the open frames' too) and those without relations. */
function cardsOf(graph: Graph): Card[] {
  const ofLevel = (level: Level): Card[] => level.flat().flatMap((n) => [n.card, ...(n.inner ? ofLevel(n.inner) : [])]);
  return [...ofLevel(graph.level), ...graph.unrelated];
}

/** A workspace's tickets as a figure of their dependencies and parents. */
export function RelationsView({ workspace }: { workspace: string }) {
  const t = useT();
  const client = useQueryClient();
  const boardQuery = useBoard(workspace);
  const members = useMembers();
  const [closedShown, showClosed] = useStored("branvil.relations.includeClosed");
  const includeClosed = closedShown === "true";
  const [opened, keepOpened] = useStored(`branvil.relations.open.${workspace}`);
  const open = useMemo(() => openParents(opened), [opened]);
  const [filter, setFilter] = useState<CardFilter>({ assignee: "", label: "", text: "" });
  const [notice, setNotice] = useState<Said | null>(null);

  // Closed tickets are read only when they are shown, or when one is the
  // parent of an open ticket (then it is shown too): an open ticket below a
  // closed ancestor has a closed parent itself or below an open one.
  const openCards = useCards(workspace, "open");
  const needAll = includeClosed || (openCards.data?.items ?? []).some((c) => c.parent?.state === "closed");
  const allCards = useCards(workspace, "all", needAll);
  // Relations change only once the cards they need are read; until then the
  // open ones are shown, and a failure to read them is shown with a retry.
  const required = needAll ? allCards : openCards;
  const shown = required.data ?? openCards.data;
  const complete = required.data !== undefined;
  const cards = useMemo(() => shown?.items ?? [], [shown]);
  const graph = useMemo(() => buildGraph(cards, { includeClosed, open }), [cards, includeClosed, open]);
  const lanes = useMemo(() => boardQuery.data?.lanes ?? [], [boardQuery.data]);
  const people = useMemo<Person[]>(
    () => (members.data?.items ?? []).filter((m) => m.kind !== "runner").map((m) => ({ name: m.name, kind: m.kind })),
    [members.data],
  );
  // Checks are loaded for the pull requests of the cards the figure shows.
  const ciUrls = useMemo(() => ciUrlsOf(cardsOf(graph)), [graph]);
  const refresh = () => void client.invalidateQueries({ queryKey: ["board"] });

  // The figure changes once the service has the change: a refusal leaves it as it was.
  const relate = useMutation({
    mutationFn: ({ change }: Write) => api.tickets.update(change.repo, change.key, change.patch),
    onMutate: () => setNotice(null),
    onSuccess: (_ticket, { done, undo }) => setNotice({ tone: "info", text: done, undo }),
    onError: (error) => setNotice({ tone: "error", error }),
    onSettled: refresh,
  });
  const move = useMutation({
    mutationFn: ({ card, lane }: { card: Card; lane: Lane }) => api.tickets.place(card.repository, card.key, { lane }),
    onMutate: () => setNotice(null),
    onSuccess: (_ticket, { card, lane }) => setNotice({ tone: "info", text: t("board.moved", { key: ticketRef(card), lane: t(`lane.${lane}`) }) }),
    onError: (err, { card }) => setNotice(moveRefusal(err, card, t)),
    onSettled: refresh,
  });

  const { mutate: write } = relate;
  const { mutate: place } = move;
  const onEdit = useCallback((edit: Edit) => write({ change: edit.change, done: t(edit.message, edit.params), undo: edit.inverse }), [write, t]);
  const onLane = useCallback((card: Card, lane: Lane) => place({ card, lane }), [place]);
  const onToggle = useCallback(
    (ref: string) => {
      const next = new Set(open);
      if (!next.delete(ref)) next.add(ref);
      keepOpened(JSON.stringify([...next]));
    },
    [open, keepOpened],
  );

  if (boardQuery.isPending) return <Loading />;
  if (boardQuery.isError) return <ErrorMessage error={boardQuery.error} />;

  return (
    <div className="workspace-fill">
      <PageHeader
        title={t("relations.title")}
        meta={
          <Text size="sm" c="dimmed">
            {t("board.summary", { workspace: boardQuery.data.name, count: cards.filter((c) => c.state === "open").length })}
          </Text>
        }
      />
      <CardFilters
        cards={cards}
        people={people}
        filter={filter}
        onFilter={setFilter}
        includeClosed={includeClosed}
        onIncludeClosed={(include) => showClosed(String(include))}
        status={required.isFetching && shown && t("common.refreshing")}
      />
      {notice && "error" in notice && <ErrorMessage error={notice.error} />}
      {notice && "text" in notice && notice.tone === "error" && <Refusal>{notice.text}</Refusal>}
      {notice && "text" in notice && notice.tone === "info" && (
        <Group gap="sm" role="status">
          <Text size="sm">{notice.text}</Text>
          {notice.undo && (
            <Button
              size="compact-sm"
              variant="default"
              disabled={relate.isPending}
              onClick={() => notice.undo && write({ change: notice.undo, done: t("relations.undone") })}
            >
              {t("relations.undo")}
            </Button>
          )}
        </Group>
      )}
      {required.isError && (
        <Group gap="sm">
          <ErrorMessage error={required.error} />
          <Button size="compact-sm" variant="default" onClick={refresh}>
            {t("common.retry")}
          </Button>
        </Group>
      )}
      {!shown ? (
        !required.isError && <Loading />
      ) : (
        <Figure
          graph={graph}
          cards={cards}
          lanes={lanes}
          people={people}
          loadChecks={ciUrls}
          filter={filter}
          open={open}
          moving={move.isPending && move.variables ? ticketRef(move.variables.card) : null}
          editable={complete && !relate.isPending}
          onToggle={onToggle}
          onLane={onLane}
          onEdit={onEdit}
        />
      )}
    </div>
  );
}
