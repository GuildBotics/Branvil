import { ActionIcon, Badge, Menu, Text } from "@mantine/core";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { ArrowDown, ArrowUp, Ellipsis, GitPullRequest } from "lucide-react";
import { memo, useState } from "react";
import * as api from "../../api/endpoints";
import { ticketRef, type Card, type Lane, type PullSummary } from "../../api/types";
import { ErrorMessage, MemberAvatar, StateBadge, type Tone } from "../../components/common";
import { CheckSlot, PeopleItems, type Person } from "../../components/PeoplePicker";
import { useT } from "../../i18n";
import { useChecks } from "../../query/hooks";
import { Link, ServiceLink } from "../../router/Router";
import { href, routeOfUrl } from "../../router/routes";
import { rollup, type CiRollup } from "./ordering";

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
 * The card's menu: its lane, its place in the lane, and its assignee, for
 * whoever cannot or does not drag. Mounted only once asked for: a menu in every
 * card costs a board of 1,700 cards seconds.
 */
function CardMenu({
  card,
  lanes,
  position,
  laneSize,
  busy,
  people,
  onMove,
  onAssign,
}: {
  card: Card;
  lanes: Lane[];
  position: number;
  laneSize: number;
  busy: boolean;
  people: Person[];
  onMove: (card: Card, lane: Lane, index: number) => void;
  onAssign: (name: string | null) => void;
}) {
  const t = useT();
  const [opened, setOpened] = useState(false);
  const ref = ticketRef(card);
  const trigger = (
    <ActionIcon variant="subtle" color="gray" size="sm" aria-label={t("board.cardMenu", { key: ref })} onClick={() => setOpened(true)} disabled={busy}>
      <Ellipsis size={16} />
    </ActionIcon>
  );
  if (!opened) return <div className="card-menu">{trigger}</div>;
  return (
    <div className="card-menu">
      <Menu opened onChange={setOpened} position="bottom-end" withinPortal>
        <Menu.Target>{trigger}</Menu.Target>
        <Menu.Dropdown>
          <Menu.Label>{t("ticket.lane")}</Menu.Label>
          {lanes.map((l) => (
            <Menu.Item
              key={l}
              leftSection={<CheckSlot on={l === card.lane} />}
              onClick={() => l !== card.lane && onMove(card, l, Number.MAX_SAFE_INTEGER)}
            >
              {t(`lane.${l}`)}
            </Menu.Item>
          ))}
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
          <Menu.Divider />
          <Menu.Label>{t("ticket.assignee")}</Menu.Label>
          <PeopleItems current={card.assignee?.name ?? null} people={people} onChoose={(name) => name !== (card.assignee?.name ?? null) && onAssign(name)} />
        </Menu.Dropdown>
      </Menu>
    </div>
  );
}

/**
 * One card. Memoised: the board passes stable props, so a change elsewhere on
 * the board (a drag position, another card, a filter) does not re-render it.
 */
export const CardView = memo(function CardView({
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
  /** URLs of the pull requests whose checks this card loads. */
  loadChecks: Set<string>;
  people: Person[];
  onMove: (card: Card, lane: Lane, index: number) => void;
}) {
  const t = useT();
  const client = useQueryClient();
  const ref = ticketRef(card);
  const assign = useMutation({
    mutationFn: (name: string | null) => (name ? api.tickets.assign(card.repository, card.key, name) : api.tickets.unassign(card.repository, card.key)),
    onSettled: () => void client.invalidateQueries({ queryKey: ["board"] }),
  });
  const assignee = card.assignee?.name;
  const kind = people.find((p) => p.name === assignee)?.kind;

  return (
    <article className={`card${busy ? " busy" : ""}`} aria-busy={busy} aria-label={`${ref} ${card.title}`}>
      <div className="card-row">
        <Link to={href.ticket(card.repository, card.key)} className="key">
          {ref}
        </Link>
        {card.state === "closed" && <StateBadge>{t("ticket.closed")}</StateBadge>}
        {card.state === "open" && card.blocked_by.some((b) => b.state === "open") && <StateBadge tone="warning">{t("board.blocked")}</StateBadge>}
        {card.children.length > 0 && (
          <Text span size="xs" c="dimmed">
            {t("board.children", { closed: card.children.filter((c) => c.state === "closed").length, total: card.children.length })}
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
        <span className="card-who">
          {assignee ? (
            <>
              <MemberAvatar name={assignee} kind={kind} size={20} />
              <span className="assignee">{assignee}</span>
            </>
          ) : (
            t("board.unassigned")
          )}
        </span>
      </div>
      {card.pull_requests.length > 0 && (
        <ul className="card-row" aria-label={t("ticket.linkedPulls")} style={{ listStyle: "none", margin: 0, padding: 0 }}>
          {card.pull_requests.map((p) => (
            <PullBadges key={p.url} pull={p} loadChecks={loadChecks.has(p.url)} />
          ))}
        </ul>
      )}
      {assign.isError && <ErrorMessage error={assign.error} />}
      <CardMenu
        card={card}
        lanes={lanes}
        position={position}
        laneSize={laneSize}
        busy={busy}
        people={people}
        onMove={onMove}
        onAssign={(name) => assign.mutate(name)}
      />
    </article>
  );
});
