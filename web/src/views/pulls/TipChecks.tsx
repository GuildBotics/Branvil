import { Accordion, Box, Group, Stack, Text } from "@mantine/core";
import { useQueries } from "@tanstack/react-query";
import { useState } from "react";
import { isApiError } from "../../api/client";
import * as api from "../../api/endpoints";
import type { Check } from "../../api/types";
import { ErrorMessage, Loading, Sha, StateBadge, type Tone } from "../../components/common";
import { useT } from "../../i18n";
import { useBranch, useRepo } from "../../query/hooks";
import { keys } from "../../query/keys";
import { Link } from "../../router/Router";
import { href } from "../../router/routes";
import { rollup, type CiRollup } from "../board/ordering";
import { CheckDrawer, CommitChecksCard, drawerTarget } from "../pull/Checks";

const CI_BUDGET = 30;
type TipStatus = CiRollup | "loading" | "error" | "empty";
const TONE: Record<TipStatus, Tone> = { passing: "success", failing: "danger", pending: "warning", not_registered: "warning", none: "neutral", loading: "neutral", error: "danger", empty: "neutral" };

/** The same commit check list as the commit page, without a second enclosing card. */
function RepositoryTipChecks({ repo }: { repo: string }) {
  const t = useT();
  const repository = useRepo(repo);
  const branch = repository.data?.default_branch ?? "";
  const tip = useBranch(repo, branch, branch !== "");
  const [check, setCheck] = useState<{ attemptId: string; hosted: Check | null } | null>(null);
  if (repository.isError) return null;
  if (tip.isError) {
    if (!isApiError(tip.error, undefined, 404)) return <ErrorMessage error={tip.error} />;
    return <Text size="sm" c="dimmed">{t("repo.noTip", { branch })}</Text>;
  }
  if (!tip.data) return <Loading />;
  return (
    <>
      <CommitChecksCard repo={repo} sha={tip.data.sha} title={t("repo.tipChecks", { branch })} linked embedded onOpen={(c) => setCheck(drawerTarget(c))} />
      <CheckDrawer repo={repo} check={check} onClose={() => setCheck(null)} />
    </>
  );
}

/** Queries stay mounted while the summary is folded, so it always follows repository events. */
function WorkspaceTipChecks({ workspace, paths }: { workspace: string; paths: string[] }) {
  const t = useT();
  const shown = paths.slice(0, CI_BUDGET);
  const repositories = useQueries({ queries: shown.map((path) => ({ queryKey: keys.repo(path), queryFn: () => api.repos.get(path) })) });
  const branches = useQueries({ queries: shown.map((path, i) => {
    const branch = repositories[i]?.data?.default_branch ?? "";
    return { queryKey: keys.branch(path, branch), queryFn: () => api.repos.branch(path, branch), enabled: branch !== "" && !repositories[i]?.isError };
  }) });
  const checks = useQueries({ queries: shown.map((path, i) => {
    const sha = branches[i]?.data?.sha ?? "";
    return { queryKey: keys.commitChecks(path, sha), queryFn: () => api.repos.commitChecks(path, sha), enabled: sha !== "" && !branches[i]?.isError && !repositories[i]?.isError };
  }) });
  const rows = shown.map((path, i) => {
    const repository = repositories[i]!;
    const branch = branches[i]!;
    const check = checks[i]!;
    const empty = branch.isError && isApiError(branch.error, undefined, 404);
    const error = repository.error ?? (empty ? null : branch.error ?? check.error);
    const status: TipStatus = error ? "error" : empty ? "empty" : check.data ? rollup(check.data) : "loading";
    return { path, branch: repository.data?.default_branch, sha: branch.data?.sha, status, error };
  });
  const statuses: TipStatus[] = ["failing", "pending", "not_registered", "passing", "none", "empty", "loading", "error"];
  const label = (status: TipStatus) => status === "loading" ? t("common.loading") : status === "error" ? t("pulls.tipError") : status === "empty" ? t("pulls.tipEmpty") : t(`ci.${status}`);
  return (
    <Accordion variant="default">
      <Accordion.Item value="default-branch-ci">
        <Accordion.Control>
          <Group gap="xs">
            <Text fw={700}>{t("pulls.tipChecks")}</Text>
            <Text size="sm" c="dimmed">{t("pulls.tipRepositories", { count: paths.length })}</Text>
            {statuses.map((status) => {
              const count = rows.filter((row) => row.status === status).length;
              return count > 0 && <StateBadge key={status} tone={TONE[status]}>{label(status)} {count}</StateBadge>;
            })}
          </Group>
        </Accordion.Control>
        <Accordion.Panel>
          <Stack gap="xs" component="section" aria-label={t("pulls.tipOfWorkspace", { workspace })}>
            {rows.map((row) => (
              <Box key={row.path}>
                <Group gap="sm">
                  <Link to={href.repo(row.path)} aria-label={row.path} style={{ flex: 1, textDecoration: "none" }}>
                    <Group gap="sm">
                      <Text span size="sm">{row.path}</Text>
                      <Text span size="sm" c="dimmed">{row.branch ?? "—"}</Text>
                      <StateBadge tone={TONE[row.status]}>{label(row.status)}</StateBadge>
                    </Group>
                  </Link>
                  {row.sha && <Link to={href.commit(row.path, row.sha)}><Sha value={row.sha} /></Link>}
                </Group>
                {row.error && <ErrorMessage error={row.error} />}
              </Box>
            ))}
          </Stack>
        </Accordion.Panel>
      </Accordion.Item>
      {paths.length > CI_BUDGET && <Text size="xs" c="dimmed" mt="xs">{t("pulls.tipCut", { count: CI_BUDGET, omitted: paths.length - CI_BUDGET })}</Text>}
    </Accordion>
  );
}

/** Default-branch CI below the pull request list, scoped by its repository selector. */
export function TipChecks({ workspace, paths, repo }: { workspace: string; paths: string[]; repo: string | null }) {
  return repo ? <RepositoryTipChecks key={repo} repo={repo} /> : <WorkspaceTipChecks workspace={workspace} paths={paths} />;
}
