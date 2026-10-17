// Choosing people with an avatar menu, as GuildBotics' MemberSelector does:
// a ticket's one assignee, and a pull request's reviewers with their state.

import { Avatar, Menu, Text, UnstyledButton } from "@mantine/core";
import { Check, MessageSquare, UserPlus, X } from "lucide-react";
import type { ReactNode } from "react";
import type { MemberKind } from "../api/types";
import { useT } from "../i18n";
import { MemberAvatar } from "./common";

export interface Person {
  name: string;
  kind?: MemberKind;
}

/** A menu item's check: the current choice is ticked, the others keep its room. */
export const CheckSlot = ({ on }: { on: boolean }) => (on ? <Check size={14} /> : <span style={{ width: 14, display: "inline-block" }} />);

function NoOne({ size }: { size: number }) {
  return (
    <Avatar size={size} radius="xl" className="avatar-none" variant="transparent" color="gray">
      <UserPlus size={Math.round(size / 2)} />
    </Avatar>
  );
}

/** One assignee: the avatar (or an empty one) opens the members; choosing one replaces it. */
export function AssigneePicker({
  current,
  people,
  onChoose,
  disabled,
}: {
  current: string | null;
  people: Person[];
  onChoose: (name: string | null) => void;
  disabled?: boolean;
}) {
  const t = useT();
  const who = people.find((p) => p.name === current) ?? (current ? { name: current } : null);
  const label = t("people.assigneeTrigger", { name: who?.name ?? t("people.nobody") });
  return (
    <Menu position="bottom-end" withinPortal>
      <Menu.Target>
        <UnstyledButton className="people-trigger" aria-label={label} title={label} disabled={disabled}>
          {who ? <MemberAvatar name={who.name} kind={who.kind} size={28} /> : <NoOne size={28} />}
        </UnstyledButton>
      </Menu.Target>
      <Menu.Dropdown>
        <PeopleItems current={current} people={people} onChoose={onChoose} />
      </Menu.Dropdown>
    </Menu>
  );
}

/** Menu items choosing one person or nobody; for a menu of its own or a section of another. */
export function PeopleItems({
  current,
  people,
  onChoose,
}: {
  current: string | null;
  people: Person[];
  onChoose: (name: string | null) => void;
}) {
  const t = useT();
  return (
    <>
      {people.map((p) => (
        <Menu.Item
          key={p.name}
          leftSection={<Lead on={p.name === current} avatar={<MemberAvatar name={p.name} kind={p.kind} size={20} />} />}
          onClick={() => onChoose(p.name)}
        >
          {p.name}
        </Menu.Item>
      ))}
      <Menu.Divider />
      <Menu.Item leftSection={<Lead on={current === null} avatar={<NoOne size={20} />} />} onClick={() => onChoose(null)}>
        {t("people.nobody")}
      </Menu.Item>
    </>
  );
}

const Lead = ({ on, avatar }: { on: boolean; avatar: ReactNode }) => (
  <span style={{ display: "inline-flex", alignItems: "center", gap: 10 }} aria-hidden>
    <CheckSlot on={on} />
    {avatar}
  </span>
);

/** A reviewer's state: an open request, or the last verdict (on the current head or an older one). */
export interface ReviewState {
  requested: boolean;
  verdict: "approve" | "request_changes" | "comment" | null;
  stale: boolean;
}

function Mark({ state }: { state: ReviewState }) {
  if (state.requested) return <span className="review-mark pending" />;
  if (!state.verdict) return null;
  const icon = state.verdict === "approve" ? <Check size={9} strokeWidth={3.5} /> : state.verdict === "request_changes" ? <X size={9} strokeWidth={3.5} /> : <MessageSquare size={8} strokeWidth={3.5} />;
  return <span className={`review-mark ${state.verdict}${state.stale ? " stale" : ""}`}>{icon}</span>;
}

function stateKey(state: ReviewState): "requested" | "requestedAgain" | "approve" | "request_changes" | "comment" {
  if (state.requested) return state.verdict ? "requestedAgain" : "requested";
  return state.verdict ?? "requested";
}

/**
 * Reviewers: everyone requested or who reviewed, each avatar marked with their
 * state. The menu requests or withdraws a review (several at once); the author
 * is not offered.
 */
export function ReviewersPicker({
  people,
  states,
  author,
  onToggle,
  disabled,
}: {
  people: Person[];
  states: Map<string, ReviewState>;
  author: string;
  onToggle: (name: string, request: boolean) => void;
  disabled?: boolean;
}) {
  const t = useT();
  const text = (s: ReviewState) => t(`people.review.${stateKey(s)}`) + (s.stale && !s.requested ? t("people.review.stale") : "");
  const involved = [...states.entries()].filter(([, s]) => s.requested || s.verdict);
  const kindOf = (name: string) => people.find((p) => p.name === name)?.kind;
  const label = t("people.reviewersTrigger", {
    names: involved.map(([n, s]) => `${n} (${text(s)})`).join(", ") || t("people.nobody"),
  });
  return (
    <Menu position="bottom-end" withinPortal closeOnItemClick={false}>
      <Menu.Target>
        <UnstyledButton className="people-trigger" aria-label={label} title={label} disabled={disabled}>
          {involved.map(([name, s]) => (
            <span key={name} className="review-mark-anchor">
              <MemberAvatar name={name} kind={kindOf(name)} size={28} />
              <Mark state={s} />
            </span>
          ))}
          <NoOne size={28} />
        </UnstyledButton>
      </Menu.Target>
      <Menu.Dropdown>
        {people
          .filter((p) => p.name !== author)
          .map((p) => {
            const s = states.get(p.name);
            const on = !!s?.requested;
            return (
              <Menu.Item
                key={p.name}
                aria-checked={on}
                leftSection={
                  <Lead
                    on={on}
                    avatar={
                      <span className="review-mark-anchor">
                        <MemberAvatar name={p.name} kind={p.kind} size={20} />
                        {s && <Mark state={s} />}
                      </span>
                    }
                  />
                }
                rightSection={
                  s && (s.requested || s.verdict) ? (
                    <Text size="xs" c="dimmed" ml="md">
                      {text(s)}
                    </Text>
                  ) : null
                }
                onClick={() => onToggle(p.name, !on)}
              >
                {p.name}
              </Menu.Item>
            );
          })}
        <Menu.Divider />
        <Menu.Label>{t("people.reviewersHelp", { author })}</Menu.Label>
      </Menu.Dropdown>
    </Menu>
  );
}
