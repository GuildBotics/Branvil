import { ActionIcon, Badge, Menu, Text, Tooltip } from "@mantine/core";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { ArrowDown, ArrowUp, Ellipsis, GitPullRequest } from "lucide-react";
import { memo, useState, type ReactElement, type ReactNode } from "react";
import { isApiError } from "../../api/client";
import * as api from "../../api/endpoints";
import { ticketRef, type Card, type Lane, type PullSummary } from "../../api/types";
import { ErrorMessage, MemberAvatar, StateBadge, type Tone } from "../../components/common";
import { CheckSlot, PeopleItems, type Person } from "../../components/PeoplePicker";
import { type Translate, useT } from "../../i18n";
import { useChecks } from "../../query/hooks";
import { Link, ServiceLink } from "../../router/Router";
import { href, routeOfUrl } from "../../router/routes";
import { relationRefusal } from "../ticket/Relations";
import { rollup, type CiRollup } from "./ordering";

/** The most pull requests whose checks a page of cards loads (one request each). */
const CI_BUDGET = 30;

/** The pull requests whose checks `cards` load: the first CI_BUDGET open ones that are this Branvil's. */
export function ciUrlsOf(cards: Card[]): Set<string> {
  const urls: string[] = [];
  for (const c of cards)
    for (const p of c.pull_requests) if (p.state === "open" && urls.length < CI_BUDGET && routeOfUrl(p.url)?.name === "pull") urls.push(p.url);
  return new Set(urls);
}

/** A line the page says about a change it made. */
export interface Notice {
  tone: "error" | "info";
  text: string;
}

/** What the page says when moving `card` to another lane or place failed. */
export function moveRefusal(err: unknown, card: Card, t: Translate): Notice {
  const key = ticketRef(card);
  // A neighbour card that moved meanwhile is answered with 404: also a concurrent change.
  if (isApiError(err, "conflict") || isApiError(err, "not_found")) return { tone: "info", text: t("board.moveConflict", { key }) };
  return { tone: "error", text: t("board.moveFailed", { key, message: relationRefusal(err, t) ?? (err as Error).message }) };
}

const CI_TONE: Record<CiRollup, Tone> = {
  passing: "success",
  failing: "danger",
  pending: "warning",
  not_registered: "warning",
  none: "neutral",
};

function PullBadges({ pull, loadChecks }: { pull: PullSummary; loadChecks: boolean }) {
  const t = useT();
  const route = routeOfUrl(pull.url);
  const isPull = route?.name === "pull";
  const checks = useChecks(isPull ? route.repo : "", isPull ? route.number : 0, isPull && loadChecks);
  const state = pull.merged ? "merged" : pull.state;
  return (
    <li className="card-row">
      <ServiceLink url={pull.url}>
        <Badge color={state === "open" ? "info" : state === "merged" ? "success" : "neutral"} variant="light" size="sm" leftSection={<GitPullRequest size={12} />} style={{ cursor: "pointer" }}>
          {pull.label} {t(`pull.state.${state}`)}
        </Badge>
      </ServiceLink>
      {checks.data && (
        <StateBadge tone={CI_TONE[rollup(checks.data)]}>
          {t("ci.label")}: {t(`ci.${rollup(checks.data)}`)}
        </StateBadge>
      )}
    </li>
  );
}

/**
 * A menu built only once its trigger is pressed: a menu in every card costs a
 * board of 1,700 cards seconds. `trigger` gets the function that opens it.
 */
export function LazyMenu({ trigger, children }: { trigger: (open: () => void) => ReactElement; children: ReactNode }) {
  const [opened, setOpened] = useState(false);
  const target = trigger(() => setOpened(true));
  if (!opened) return target;
  return (
    <Menu opened onChange={setOpened} position="bottom-end" withinPortal>
      <Menu.Target>{target}</Menu.Target>
      <Menu.Dropdown>{children}</Menu.Dropdown>
    </Menu>
  );
}

/** Menu items choosing a lane; choosing the current one does nothing. */
export function LaneItems({ lanes, current, onChoose }: { lanes: Lane[]; current: Lane; onChoose: (lane: Lane) => void }) {
  const t = useT();
  return lanes.map((l) => (
    <Menu.Item key={l} leftSection={<CheckSlot on={l === current} />} onClick={() => l !== current && onChoose(l)}>
      {t(`lane.${l}`)}
    </Menu.Item>
  ));
}

/** A card's lane, pressed to move the card to another one. */
export function LaneButton({ card, lanes, onLane, disabled }: { card: Card; lanes: Lane[]; onLane: (card: Card, lane: Lane) => void; disabled?: boolean }) {
  const t = useT();
  const lane = t(`lane.${card.lane}`);
  return (
    <LazyMenu
      trigger={(open) => (
        <button type="button" className="card-control" aria-label={t("relations.lane", { key: ticketRef(card), lane })} onClick={open} disabled={disabled}>
          {lane}
        </button>
      )}
    >
      <LaneItems lanes={lanes} current={card.lane} onChoose={(l) => onLane(card, l)} />
    </LazyMenu>
  );
}

const childrenText = (card: Card, t: Translate) =>
  t("board.children", { closed: card.children.filter((c) => c.state === "closed").length, total: card.children.length });

/** Opens or closes a card's children (or the frame of new tickets'). */
export function ChildrenToggle({ open, onToggle }: { open: boolean; onToggle: () => void }) {
  const t = useT();
  const label = t(open ? "relations.closeChildren" : "relations.openChildren");
  return (
    <Tooltip label={label}>
      <button type="button" className="card-control" aria-label={label} aria-expanded={open} onClick={onToggle}>
        {open ? "▼" : "▶"}
      </button>
    </Tooltip>
  );
}

/**
 * One card: its key, state, title, labels, pull requests, and its assignee,
 * pressed to change. `lead` comes before the key (the toggle of a parent's
 * children), `lane` follows it, and `children` are laid over the card (its
 * menu).
 */
export function CardView({
  card,
  people,
  loadChecks,
  busy,
  className,
  lane,
  lead,
  children,
}: {
  card: Card;
  people: Person[];
  /** URLs of the pull requests whose checks this card loads. */
  loadChecks: Set<string>;
  busy: boolean;
  className?: string;
  lane?: ReactNode;
  lead?: ReactNode;
  children?: ReactNode;
}) {
  const t = useT();
  const client = useQueryClient();
  const ref = ticketRef(card);
  const assign = useMutation({
    mutationFn: (name: string | null) => (name ? api.tickets.assign(card.repository, card.key, name) : api.tickets.unassign(card.repository, card.key)),
    onSettled: () => void client.invalidateQueries({ queryKey: ["board"] }),
  });
  const assignee = card.assignee?.name ?? null;
  const kind = people.find((p) => p.name === assignee)?.kind;

  return (
    <article className={["card", busy && "busy", className].filter(Boolean).join(" ")} aria-busy={busy} aria-label={`${ref} ${card.title}`}>
      <div className="card-row">
        {lead}
        <Link to={href.ticket(card.repository, card.key)} className="key">
          {ref}
        </Link>
        {lane}
        {card.state === "closed" && <StateBadge>{t("ticket.closed")}</StateBadge>}
        {card.state === "open" && card.blocked_by.some((b) => b.state === "open") && <StateBadge tone="warning">{t("board.blocked")}</StateBadge>}
        {card.children.length > 0 && (
          <Text span size="xs" c="dimmed">
            {childrenText(card, t)}
          </Text>
        )}
        {busy && (
          <Text span size="xs" c="dimmed">
            {t("board.moving")}
          </Text>
        )}
      </div>
      <p className="title">
        <Link to={href.ticket(card.repository, card.key)}>{card.title}</Link>
      </p>
      <div className="card-row">
        {card.labels.length > 0 && (
          <ul className="card-row" aria-label={t("ticket.labels")} style={{ listStyle: "none", margin: 0, padding: 0 }}>
            {card.labels.map((l) => (
              <li key={l}>
                <Badge variant="outline" size="sm">
                  {l}
                </Badge>
              </li>
            ))}
          </ul>
        )}
        <LazyMenu
          trigger={(open) => (
            <button
              type="button"
              className="card-control card-who"
              aria-label={t("board.assignee", { key: ref, name: assignee ?? t("board.unassigned") })}
              onClick={open}
              disabled={busy}
            >
              {assignee ? (
                <>
                  <MemberAvatar name={assignee} kind={kind} size={20} />
                  <span className="assignee">{assignee}</span>
                </>
              ) : (
                t("board.unassigned")
              )}
            </button>
          )}
        >
          <PeopleItems current={assignee} people={people} onChoose={(name) => name !== assignee && assign.mutate(name)} />
        </LazyMenu>
      </div>
      {card.pull_requests.length > 0 && (
        <ul className="card-row" aria-label={t("ticket.linkedPulls")} style={{ listStyle: "none", margin: 0, padding: 0 }}>
          {card.pull_requests.map((p) => (
            <PullBadges key={p.url} pull={p} loadChecks={loadChecks.has(p.url)} />
          ))}
        </ul>
      )}
      {assign.isError && <ErrorMessage error={assign.error} />}
      {children}
    </article>
  );
}

/** The board's menu of a card: its lane and its place in the lane, for whoever cannot or does not drag. */
function CardMenu({
  card,
  lanes,
  position,
  laneSize,
  busy,
  onMove,
}: {
  card: Card;
  lanes: Lane[];
  position: number;
  laneSize: number;
  busy: boolean;
  onMove: (card: Card, lane: Lane, index: number) => void;
}) {
  const t = useT();
  const ref = ticketRef(card);
  return (
    <div className="card-menu">
      <LazyMenu
        trigger={(open) => (
          <ActionIcon variant="subtle" color="gray" size="sm" aria-label={t("board.cardMenu", { key: ref })} onClick={open} disabled={busy}>
            <Ellipsis size={16} />
          </ActionIcon>
        )}
      >
        <Menu.Label>{t("ticket.lane")}</Menu.Label>
        <LaneItems lanes={lanes} current={card.lane} onChoose={(l) => onMove(card, l, Number.MAX_SAFE_INTEGER)} />
        <Menu.Divider />
        <Menu.Item
          leftSection={<ArrowUp size={14} />}
          aria-label={t("board.moveUp", { key: ref })}
          disabled={position <= 0}
          onClick={() => onMove(card, card.lane, position - 1)}
        >
          {t("board.up")}
        </Menu.Item>
        <Menu.Item
          leftSection={<ArrowDown size={14} />}
          aria-label={t("board.moveDown", { key: ref })}
          disabled={position >= laneSize - 1}
          onClick={() => onMove(card, card.lane, position + 1)}
        >
          {t("board.down")}
        </Menu.Item>
      </LazyMenu>
    </div>
  );
}

/**
 * A card on the board, with its menu. Memoised: the board passes stable props,
 * so a change elsewhere on the board (a drag position, another card, a filter)
 * does not re-render it.
 */
export const BoardCard = memo(function BoardCard({
  card,
  lanes,
  position,
  laneSize,
  busy,
  loadChecks,
  people,
  onMove,
}: {
  card: Card;
  lanes: Lane[];
  position: number;
  laneSize: number;
  busy: boolean;
  loadChecks: Set<string>;
  people: Person[];
  onMove: (card: Card, lane: Lane, index: number) => void;
}) {
  return (
    <CardView card={card} people={people} loadChecks={loadChecks} busy={busy}>
      <CardMenu card={card} lanes={lanes} position={position} laneSize={laneSize} busy={busy} onMove={onMove} />
    </CardView>
  );
});
