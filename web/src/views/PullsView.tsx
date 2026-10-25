import { Card, Group, Select, Text, TextInput } from "@mantine/core";
import { useQueries } from "@tanstack/react-query";
import { Search } from "lucide-react";
import { useMemo, useState } from "react";
import * as api from "../api/endpoints";
import type { PullRequest } from "../api/types";
import { ErrorMessage, Loading, MemberAvatar, StateBadge, Time, type Tone } from "../components/common";
import { PageHeader } from "../components/PageHeader";
import { useT } from "../i18n";
import { useChecks, useMemberKinds, useRepos } from "../query/hooks";
import { keys } from "../query/keys";
import { Link, navigate } from "../router/Router";
import { href } from "../router/routes";
import { rollup, type CiRollup } from "./board/ordering";
import { LandingBadge } from "./pull/LandingStatus";
import { LandingQueue } from "./pulls/LandingQueue";
import { TipChecks } from "./pulls/TipChecks";
import { ReviewersCell } from "./pulls/ReviewersCell";

type State = "open" | "closed" | "all";
const ALL_REPOS = "*";

/** The most rows whose checks are read (one request each), as the board limits them. */
const CI_BUDGET = 30;

const CI_TONE: Record<CiRollup, Tone> = { passing: "success", failing: "danger", pending: "warning", not_registered: "warning", none: "neutral" };

function CiCell({ pull, load }: { pull: PullRequest; load: boolean }) {
  const t = useT();
  const checks = useChecks(pull.repository, pull.number, load);
  if (!checks.data) return null;
  return <StateBadge tone={CI_TONE[rollup(checks.data)]}>{t(`ci.${rollup(checks.data)}`)}</StateBadge>;
}

/**
 * The pull requests of a workspace's repositories; a repository's page
 * (`/{repo}`) is the checks of its default branch and this list for that
 * repository.
 */
export function PullsView({ workspace, repo }: { workspace: string; repo: string | null }) {
  const t = useT();
  const repos = useRepos();
  const kindOf = useMemberKinds();
  const [state, setState] = useState<State>("open");
  const [text, setText] = useState("");
  const repoOf = (path: string) => repos.data?.items.find((r) => r.path === path);
  const paths = useMemo(
    () => (repos.data?.items ?? []).map((r) => r.path).filter((p) => p.startsWith(`${workspace}/`)),
    [repos.data, workspace],
  );
  const shown = repo ? [repo] : paths;
  const lists = useQueries({
    queries: shown.map((path) => ({ queryKey: keys.repoPulls(path, state), queryFn: () => api.repos.pulls(path, state) })),
  });
  const pending = repos.isPending || lists.some((q) => q.isPending);
  const failed = repos.error ?? lists.find((q) => q.isError)?.error;
  const cut = lists.some((q) => q.data?.next_cursor);
  const needle = text.trim().toLowerCase();
  const pulls = lists
    .flatMap((q) => q.data?.items ?? [])
    .filter((p) => !needle || `${p.repository} ${p.label} ${p.title}`.toLowerCase().includes(needle))
    .sort((a, b) => (a.updated_at < b.updated_at ? 1 : a.updated_at > b.updated_at ? -1 : 0));

  return (
    <>
      <PageHeader
        title={t("pulls.title")}
        meta={
          <Text size="sm" c="dimmed">
            {repo ?? t("pulls.ofWorkspace", { workspace })}
          </Text>
        }
      />
      {repo && <LandingQueue repo={repo} />}
      <Card p="lg">
        <Group gap="sm" mb={14} role="search" aria-label={t("pulls.filters")}>
          <TextInput
            aria-label={t("board.filterText")}
            placeholder={t("pulls.filterPlaceholder")}
            leftSection={<Search size={16} />}
            value={text}
            onChange={(e) => setText(e.currentTarget.value)}
            style={{ flex: "1 1 260px", maxWidth: 380 }}
          />
          <Select
            aria-label={t("pulls.repository")}
            w={220}
            value={repo ?? ALL_REPOS}
            allowDeselect={false}
            onChange={(v) => v && navigate(v === ALL_REPOS ? href.pulls(workspace) : href.repo(v))}
            data={[{ value: ALL_REPOS, label: `${t("pulls.repository")}: ${t("board.any")}` }, ...paths.map((p) => ({ value: p, label: p }))]}
          />
          <Select
            aria-label={t("pulls.state")}
            w={170}
            value={state}
            allowDeselect={false}
            onChange={(v) => v && setState(v as State)}
            data={[
              { value: "open", label: `${t("pulls.state")}: ${t("pull.state.open")}` },
              { value: "closed", label: `${t("pulls.state")}: ${t("pull.state.closed")}` },
              { value: "all", label: `${t("pulls.state")}: ${t("board.any")}` },
            ]}
          />
        </Group>
        {failed ? (
          <ErrorMessage error={failed} />
        ) : pending ? (
          <Loading />
        ) : (
          <div className="list pull-list" role="table" aria-label={t("pulls.title")}>
            <div className="list-header pull-cols" role="row">
              <span role="columnheader">{t("pulls.repository")}</span>
              <span role="columnheader">PR</span>
              <span role="columnheader">{t("ticket.title")}</span>
              <span role="columnheader">{t("pulls.state")}</span>
              <span role="columnheader">{t("ci.label")}</span>
              <span role="columnheader">{t("ticket.author")}</span>
              <span role="columnheader">{t("member.reviewers")}</span>
              <span role="columnheader">{t("pulls.updated")}</span>
            </div>
            {pulls.length === 0 ? (
              <div className="empty-row">{state === "open" ? t("pulls.noneOpen") : t("pulls.none")}</div>
            ) : (
              pulls.map((p, i) => {
                const s = p.merged ? "merged" : p.state;
                return (
                  <Link key={p.url} to={href.pull(p.repository, p.number)} className="list-row pull-cols" role="row" aria-label={`${p.repository} ${p.label} ${p.title}`}>
                    <Text span size="sm" c="dimmed" role="cell">
                      {p.repository}
                    </Text>
                    <span className="mono" role="cell">
                      {p.label}
                    </span>
                    <span role="cell">{p.title}</span>
                    <Group gap={4} role="cell" wrap="nowrap">
                      <StateBadge tone={s === "closed" ? "neutral" : "success"}>{t(`pull.state.${s}`)}</StateBadge>
                      {p.draft && <StateBadge tone="warning">{t("pull.draft")}</StateBadge>}
                      {p.state === "open" && p.landing && <LandingBadge landing={p.landing} host={repoOf(p.repository)?.upstream != null} />}
                    </Group>
                    <span role="cell">{p.state === "open" && <CiCell pull={p} load={i < CI_BUDGET} />}</span>
                    <Group gap={6} role="cell" wrap="nowrap">
                      <MemberAvatar name={p.author} kind={kindOf(p.author)} size={20} />
                      <Text span size="sm" truncate style={{ minWidth: 0 }} title={p.author}>
                        {p.author}
                      </Text>
                    </Group>
                    <span role="cell">
                      <ReviewersCell pull={p} />
                    </span>
                    <Text span size="xs" c="dimmed" role="cell">
                      <Time value={p.updated_at} />
                    </Text>
                  </Link>
                );
              })
            )}
          </div>
        )}
        {cut && (
          <Text size="xs" c="dimmed" mt="sm">
            {t("pulls.cut")}
          </Text>
        )}
        <TipChecks key={repo ?? ALL_REPOS} workspace={workspace} paths={paths} repo={repo} />
      </Card>
    </>
  );
}
