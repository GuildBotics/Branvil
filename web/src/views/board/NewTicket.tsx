// Adding a ticket to a lane, as GitHub's project boards do: a title typed at
// the foot of the lane, or a modal for its body and labels too.

import { Anchor, Button, Chip, Group, Modal, Select, Stack, Text, Textarea, TextInput, UnstyledButton } from "@mantine/core";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Plus } from "lucide-react";
import { useState } from "react";
import * as api from "../../api/endpoints";
import { ticketRef, type Lane, type Repository, type Ticket } from "../../api/types";
import { ErrorMessage, Notice } from "../../components/common";
import { useT } from "../../i18n";
import { keys } from "../../query/keys";

interface Draft {
  repo: string;
  title: string;
  body: string;
  labels: string[];
}

function useCreate(workspace: string, onCreated: (t: Ticket) => void) {
  const client = useQueryClient();
  return useMutation({
    mutationFn: ({ draft, lane }: { draft: Draft; lane: Lane }) =>
      api.tickets.create(draft.repo, { title: draft.title.trim(), body: draft.body, labels: draft.labels, lane }),
    onSuccess: onCreated,
    onSettled: () => void client.invalidateQueries({ queryKey: keys.board(workspace) }),
  });
}

/** Chooses among the workspace's repositories; shown only when there is more than one. */
function RepoChoice({ repos, value, onChange, size }: { repos: Repository[]; value: string; onChange: (r: string) => void; size?: "xs" }) {
  const t = useT();
  if (repos.length < 2) return null;
  return (
    <Select
      label={size ? undefined : t("board.repository")}
      aria-label={t("board.repository")}
      size={size}
      data={repos.map((r) => r.path)}
      value={value}
      onChange={(v) => v && onChange(v)}
      allowDeselect={false}
    />
  );
}

/** The foot of a lane: "Add ticket", which opens a title field in place. */
export function LaneAdd({
  workspace,
  lane,
  repos,
  repo,
  onRepo,
  onMore,
}: {
  workspace: string;
  lane: Lane;
  repos: Repository[];
  repo: string;
  onRepo: (repo: string) => void;
  onMore: (lane: Lane, title: string) => void;
}) {
  const t = useT();
  const [open, setOpen] = useState(false);
  const [title, setTitle] = useState("");
  const [created, setCreated] = useState<string | null>(null);
  const create = useCreate(workspace, (tk) => {
    setTitle("");
    setCreated(ticketRef(tk));
  });
  const submit = () => {
    if (!title.trim() || !repo) return;
    create.mutate({ draft: { repo, title, body: "", labels: [] }, lane });
  };
  const close = () => {
    setOpen(false);
    setTitle("");
    setCreated(null);
    create.reset();
  };
  const laneName = t(`lane.${lane}`);
  return (
    <div className="lane-foot">
      {!open ? (
        <UnstyledButton
          className="nav-item"
          style={{ minHeight: 32, width: "100%", fontSize: 14 }}
          onClick={() => setOpen(true)}
          disabled={!repo}
          aria-label={t("board.addToLane", { lane: laneName })}
        >
          <Plus size={16} />
          {t("board.add")}
        </UnstyledButton>
      ) : (
        <form
          className="lane-composer"
          aria-label={t("board.addToLane", { lane: laneName })}
          onSubmit={(e) => {
            e.preventDefault();
            submit();
          }}
        >
          <RepoChoice repos={repos} value={repo} onChange={onRepo} size="xs" />
          <Textarea
            aria-label={t("ticket.title")}
            placeholder={t("ticket.title")}
            value={title}
            onChange={(e) => {
              setTitle(e.currentTarget.value);
              setCreated(null);
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
                e.preventDefault();
                submit();
              } else if (e.key === "Escape") close();
            }}
            rows={2}
            autoFocus
          />
          {create.isError && <ErrorMessage error={create.error} />}
          {created && <Notice>{t("board.created", { key: created })}</Notice>}
          <Group gap={6} wrap="nowrap" justify="flex-end">
            <Button size="xs" variant="default" onClick={close}>
              {t("common.cancel")}
            </Button>
            <Button size="xs" type="submit" disabled={create.isPending || !title.trim()}>
              {t("board.addSubmit")}
            </Button>
          </Group>
          <Anchor component="button" type="button" size="xs" ta="left" onClick={() => onMore(lane, title)}>
            {t("board.more")}
          </Anchor>
        </form>
      )}
    </div>
  );
}

/** A new ticket with its body and labels, in the lane it was asked for. */
export function NewTicketModal({
  workspace,
  lanes,
  start,
  repos,
  repo,
  onRepo,
  onClose,
}: {
  workspace: string;
  lanes: Lane[];
  /** The lane and title it opened with; null while closed. */
  start: { lane: Lane; title: string } | null;
  repos: Repository[];
  repo: string;
  onRepo: (repo: string) => void;
  onClose: () => void;
}) {
  const t = useT();
  const [lane, setLane] = useState<Lane | null>(null);
  const [draft, setDraft] = useState<Omit<Draft, "repo"> | null>(null);
  const current = draft ?? { title: start?.title ?? "", body: "", labels: [] };
  const chosenLane = lane ?? start?.lane ?? lanes[0] ?? "new";
  const repository = repos.find((r) => r.path === repo);
  const create = useCreate(workspace, () => close());
  function close() {
    setLane(null);
    setDraft(null);
    create.reset();
    onClose();
  }
  return (
    <Modal
      opened={start !== null}
      onClose={close}
      title={t("board.newTicketIn", { lane: t(`lane.${chosenLane}`) })}
      closeButtonProps={{ "aria-label": t("common.close") }}
      centered
      size="lg"
    >
      <form
        aria-label={t("board.newTicket")}
        onSubmit={(e) => {
          e.preventDefault();
          create.mutate({ draft: { ...current, repo }, lane: chosenLane });
        }}
      >
        <Stack gap="md">
          <Group grow align="flex-start">
            {repos.length > 1 && <RepoChoice repos={repos} value={repo} onChange={(r) => {
              onRepo(r);
              setDraft({ ...current, labels: [] });
            }} />}
            <Select
              label={t("ticket.lane")}
              data={lanes.map((l) => ({ value: l, label: t(`lane.${l}`) }))}
              value={chosenLane}
              onChange={(v) => v && setLane(v as Lane)}
              allowDeselect={false}
            />
          </Group>
          <TextInput label={t("ticket.title")} value={current.title} onChange={(e) => setDraft({ ...current, title: e.currentTarget.value })} required data-autofocus />
          <Textarea label={t("ticket.body")} value={current.body} onChange={(e) => setDraft({ ...current, body: e.currentTarget.value })} rows={6} />
          {repository && repository.labels.length > 0 && (
            <Stack gap={6}>
              <Text size="sm" fw={500}>
                {t("ticket.labels")}
              </Text>
              <Chip.Group multiple value={current.labels} onChange={(labels) => setDraft({ ...current, labels })}>
                <Group gap={6}>
                  {repository.labels.map((l) => (
                    <Chip key={l} value={l} size="xs">
                      {l}
                    </Chip>
                  ))}
                </Group>
              </Chip.Group>
            </Stack>
          )}
          {create.isError && <ErrorMessage error={create.error} />}
          <Group justify="flex-end">
            <Button variant="default" onClick={close}>
              {t("common.cancel")}
            </Button>
            <Button type="submit" disabled={create.isPending || !current.title.trim() || !repo}>
              {t("board.create")}
            </Button>
          </Group>
        </Stack>
      </form>
    </Modal>
  );
}
