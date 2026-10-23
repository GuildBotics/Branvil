// Choosing people with an avatar menu, as GuildBotics' MemberSelector does:
// a ticket's one assignee, a pull request's reviewers with their state, and
// the reviewers a member's pull requests ask.

import { Avatar, Menu, Text, UnstyledButton } from "@mantine/core";
import { Check, MessageSquare, UserPlus, X } from "lucide-react";
import type { ReactNode } from "react";
import type { MemberKind, ReviewState } from "../api/types";
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

function Mark({ state }: { state: ReviewState }) {
  if (state.requested) return <span className="review-mark pending" />;
  if (!state.verdict) return null;
  const icon = state.verdict === "approve" ? <Check size={9} strokeWidth={3.5} /> : state.verdict === "request_changes" ? <X size={9} strokeWidth={3.5} /> : <MessageSquare size={8} strokeWidth={3.5} />;
  return <span className={`review-mark ${state.verdict}${state.outdated ? " stale" : ""}`}>{icon}</span>;
}

function stateKey(state: ReviewState): "requested" | "requestedAgain" | "approve" | "request_changes" | "comment" {
  if (state.requested) return state.verdict ? "requestedAgain" : "requested";
  return state.verdict ?? "requested";
}

/**
 * Several people: the avatars of those `shown` (each with its `mark`) and an
 * empty one open the menu, where choosing a person toggles them (the menu
 * stays open). `except` is not offered. The menu opens from the trigger's
 * `position` edge, which stays put as avatars come and go.
 */
export function PeopleToggle({
  people,
  except,
  shown,
  on,
  mark,
  note,
  label,
  help,
  onToggle,
  disabled,
  position = "bottom-end",
}: {
  people: Person[];
  except: string;
  shown: string[];
  on: (name: string) => boolean;
  mark?: (name: string) => ReactNode;
  note?: (name: string) => string | null;
  label: string;
  help: string;
  onToggle: (name: string, on: boolean) => void;
  disabled?: boolean;
  position?: "bottom-start" | "bottom-end";
}) {
  const kindOf = (name: string) => people.find((p) => p.name === name)?.kind;
  const avatar = (name: string, size: number) => (
    <span key={name} className="review-mark-anchor">
      <MemberAvatar name={name} kind={kindOf(name)} size={size} />
      {mark?.(name)}
    </span>
  );
  return (
    <Menu position={position} withinPortal closeOnItemClick={false}>
      <Menu.Target>
        <UnstyledButton className="people-trigger" aria-label={label} title={label} disabled={disabled}>
          {shown.map((name) => avatar(name, 28))}
          <NoOne size={28} />
        </UnstyledButton>
      </Menu.Target>
      <Menu.Dropdown>
        {people
          .filter((p) => p.name !== except)
          .map((p) => {
            const checked = on(p.name);
            const text = note?.(p.name);
            return (
              <Menu.Item
                key={p.name}
                aria-checked={checked}
                leftSection={<Lead on={checked} avatar={avatar(p.name, 20)} />}
                rightSection={
                  text ? (
                    <Text size="xs" c="dimmed" ml="md">
                      {text}
                    </Text>
                  ) : null
                }
                onClick={() => onToggle(p.name, !checked)}
              >
                {p.name}
              </Menu.Item>
            );
          })}
        <Menu.Divider />
        <Menu.Label>{help}</Menu.Label>
      </Menu.Dropdown>
    </Menu>
  );
}

/**
 * Reviewers: everyone requested or who reviewed, each avatar marked with the
 * state the server gives. The menu requests or withdraws a review (several at
 * once); the author is not offered.
 */
export function ReviewersPicker({
  people,
  states,
  author,
  onToggle,
  disabled,
}: {
  people: Person[];
  states: ReviewState[];
  author: string;
  onToggle: (name: string, request: boolean) => void;
  disabled?: boolean;
}) {
  const t = useT();
  const text = (s: ReviewState) => t(`people.review.${stateKey(s)}`) + (s.outdated && !s.requested ? t("people.review.stale") : "");
  const state = (name: string) => states.find((s) => s.member === name);
  return (
    <PeopleToggle
      people={people}
      except={author}
      shown={states.map((s) => s.member)}
      on={(name) => !!state(name)?.requested}
      mark={(name) => {
        const s = state(name);
        return s && <Mark state={s} />;
      }}
      note={(name) => {
        const s = state(name);
        return s ? text(s) : null;
      }}
      label={t("people.reviewersTrigger", {
        names: states.map((s) => `${s.member} (${text(s)})`).join(", ") || t("people.nobody"),
      })}
      help={t("people.reviewersHelp", { author })}
      onToggle={onToggle}
      disabled={disabled}
    />
  );
}
