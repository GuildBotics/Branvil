// The checks of a pull request's head or of a commit: rows on the page, and one
// check's details, actions, and log in a drawer.

import { Anchor, Button, Card, Drawer, Group, Select, Stack, Text } from "@mantine/core";
import { useMutation, useQueryClient, type UseQueryResult } from "@tanstack/react-query";
import { ChevronRight, CircleCheck, CircleDashed, CircleX, ExternalLink } from "lucide-react";
import { useState } from "react";
import * as api from "../../api/endpoints";
import type { Check, CheckSet } from "../../api/types";
import { ErrorMessage, Loading, Sha, StateBadge, Time, type Tone } from "../../components/common";
import { useT } from "../../i18n";
import { useChecks, useCommitChecks, useJob, useJobLog } from "../../query/hooks";
import { keys } from "../../query/keys";
import { Link } from "../../router/Router";
import { href } from "../../router/routes";
import { rollup, type CiRollup } from "../board/ordering";

export const TAIL_SIZES = [16 * 1024, 64 * 1024, 256 * 1024];

const FAILED = new Set(["failure", "cancelled", "timed_out", "startup_failure"]);

function toneOf(c: Pick<Check, "status" | "conclusion">): Tone {
  if (c.status !== "completed") return "warning";
  if (c.conclusion === "success") return "success";
  if (c.conclusion && FAILED.has(c.conclusion)) return "danger";
  return "neutral";
}

const ROLLUP_TONE: Record<CiRollup, Tone> = { passing: "success", failing: "danger", pending: "warning", not_registered: "warning", none: "neutral" };

export function CheckStatusBadge({ status, conclusion }: Pick<Check, "status" | "conclusion">) {
  const t = useT();
  const label = status === "completed" && conclusion ? t(`ci.conclusion.${conclusion}`) : t(`ci.status.${status}`);
  return <StateBadge tone={toneOf({ status, conclusion })}>{label}</StateBadge>;
}

function StatusIcon({ check }: { check: Check }) {
  const tone = toneOf(check);
  if (tone === "success") return <CircleCheck size={16} color="var(--color-success)" />;
  if (tone === "danger") return <CircleX size={16} color="var(--color-danger)" />;
  return <CircleDashed size={16} color="var(--color-warning)" />;
}

/** A check of the upstream host has an id but no Branvil attempt; its page is on the host. */
const isHosted = (c: Check) => !!c.attempt_id && c.attempt === undefined;

/** The checks of the pull request's head; a row opens its drawer. */
export function ChecksCard({ repo, number, requiredChecks, onOpen }: { repo: string; number: number; requiredChecks: string[]; onOpen: (check: Check) => void }) {
  const t = useT();
  const checks = useChecks(repo, number);
  const behind = checks.data?.behind_by ?? 0;
  return (
    <CheckList repo={repo} title={t("pull.checks")} query={checks} sha={checks.data?.head_sha} requiredChecks={requiredChecks} onOpen={onOpen}>
      {behind > 0 && (
        <Text size="sm" c="var(--color-warning-text)" mb="xs">
          {t("pull.behindBy", { count: behind })}
        </Text>
      )}
    </CheckList>
  );
}

/** The checks of a commit; a row opens its drawer. `linked` links the commit's page. */
export function CommitChecksCard({ repo, sha, title, linked, onOpen }: { repo: string; sha: string; title: string; linked?: boolean; onOpen: (check: Check) => void }) {
  const checks = useCommitChecks(repo, sha);
  return <CheckList repo={repo} title={title} query={checks} sha={checks.data?.sha} linked={linked} onOpen={onOpen} />;
}

function CheckList({
  repo,
  title,
  query,
  sha,
  linked,
  requiredChecks = [],
  onOpen,
  children,
}: {
  repo: string;
  title: string;
  query: UseQueryResult<CheckSet>;
  sha: string | undefined;
  linked?: boolean;
  requiredChecks?: string[];
  onOpen: (check: Check) => void;
  children?: React.ReactNode;
}) {
  const t = useT();
  return (
    <Card p="lg" component="section" aria-label={title}>
      <Group gap="xs" mb="xs">
        <Text fw={700}>{title}</Text>
        {query.data && <StateBadge tone={ROLLUP_TONE[rollup(query.data)]}>{t(`ci.${rollup(query.data)}`)}</StateBadge>}
        {sha !== undefined && (
          <Text size="xs" c="dimmed" ml="auto">
            {t("pull.checksOn")}{" "}
            {linked && sha ? (
              <Link to={href.commit(repo, sha)}>
                <Sha value={sha} />
              </Link>
            ) : (
              <Sha value={sha} />
            )}
          </Text>
        )}
      </Group>
      {query.isPending ? (
        <Loading />
      ) : query.isError ? (
        <ErrorMessage error={query.error} />
      ) : (
        <>
          {children}
          {query.data.checks.length === 0 ? (
            <Text size="sm" c="dimmed">
              {query.data.checks_expected ? t("ci.notRegisteredHelp") : t("ci.noneHelp")}
            </Text>
          ) : (
            <ul style={{ listStyle: "none", margin: 0, padding: 0 }}>
              {query.data.checks.map((c) => (
                <li key={`${c.name}#${c.attempt_id ?? "pending"}`}>
                  <CheckRow check={c} repo={repo} required={requiredChecks.includes(c.name)} onOpen={onOpen} />
                </li>
              ))}
            </ul>
          )}
        </>
      )}
    </Card>
  );
}

function CheckRow({ check, repo, required, onOpen }: { check: Check; repo: string; required: boolean; onOpen: (check: Check) => void }) {
  const t = useT();
  const failed = check.status === "completed" && !!check.conclusion && FAILED.has(check.conclusion);
  // A failed attempt's reason (`details`) is shown in the row.
  const job = useJob(repo, check.attempt_id ?? "", !!check.attempt_id && !isHosted(check) && failed);
  const reason = isHosted(check) ? check.details : (job.data?.details ?? check.details);
  const content = (
    <>
      <StatusIcon check={check} />
      <strong>{check.name}</strong>
      {required && <StateBadge tone="neutral">{t("ci.required")}</StateBadge>}
      {check.attempt !== undefined && (
        <Text span size="sm" c="dimmed">
          #{check.attempt}
        </Text>
      )}
      <span style={{ marginLeft: "auto" }} />
      <CheckStatusBadge status={check.status} conclusion={check.conclusion} />
      {check.attempt_id && <ChevronRight size={16} color="var(--mantine-color-dimmed)" />}
    </>
  );
  return (
    <>
      {check.attempt_id ? (
        <button type="button" className="check-row" onClick={() => onOpen(check)} aria-label={t("ci.open", { name: check.name })}>
          {content}
        </button>
      ) : (
        <div className="check-row" style={{ cursor: "default" }}>
          {content}
        </div>
      )}
      {reason && (
        <Text size="xs" c={failed ? "var(--color-danger-text)" : "dimmed"} ml={24} mb={4}>
          {t("ci.reason")}: {reason}
        </Text>
      )}
    </>
  );
}

/** A bounded tail of a CI attempt's log; the user may ask for a larger tail. */
export function LogViewer({ repo, attemptId, action }: { repo: string; attemptId: string; action?: React.ReactNode }) {
  const t = useT();
  const [tail, setTail] = useState(TAIL_SIZES[0] as number);
  const log = useJobLog(repo, attemptId, tail);
  return (
    <Stack gap="xs">
      <Group gap="xs">
        {action}
        <Select
          ml="auto"
          size="xs"
          w={140}
          aria-label={t("ci.tailSize")}
          value={String(tail)}
          onChange={(v) => v && setTail(Number(v))}
          allowDeselect={false}
          data={TAIL_SIZES.map((s) => ({ value: String(s), label: t("ci.tail", { size: s / 1024 }) }))}
        />
      </Group>
      {log.isPending ? (
        <Loading />
      ) : log.isError ? (
        <ErrorMessage error={log.error} />
      ) : (
        <>
          {log.data.truncated && (
            <Text size="xs" c="dimmed">
              {t("ci.truncated", { shown: Math.min(tail, log.data.log_bytes), total: log.data.log_bytes })}
            </Text>
          )}
          <pre className="log-text" tabIndex={0} aria-label={t("ci.log")}>
            {log.data.log || t("ci.emptyLog")}
          </pre>
        </>
      )}
    </Stack>
  );
}

/** One attempt of a Branvil job: status, commit, times, reason, artifacts, retry/cancel, log. */
function JobDetails({ repo, attemptId, onAttempt }: { repo: string; attemptId: string; onAttempt: (id: string) => void }) {
  const t = useT();
  const client = useQueryClient();
  const job = useJob(repo, attemptId);
  const settle = () => {
    void client.invalidateQueries({ queryKey: keys.job(repo, attemptId) });
    void client.invalidateQueries({ queryKey: ["repo", repo, "pull"] });
    if (job.data) void client.invalidateQueries({ queryKey: keys.commitChecks(repo, job.data.sha) });
  };
  const retry = useMutation({ mutationFn: () => api.jobs.retry(repo, attemptId), onSuccess: (j) => onAttempt(j.id), onSettled: settle });
  const cancel = useMutation({ mutationFn: () => api.jobs.cancel(repo, attemptId), onSettled: settle });
  if (job.isPending) return <Loading />;
  if (job.isError) return <ErrorMessage error={job.error} />;
  const j = job.data;
  return (
    <Stack gap="md">
      <dl>
        <dt>{t("ci.commit")}</dt>
        <dd>
          <Link to={href.commit(repo, j.sha)}>
            <Sha value={j.sha} />
          </Link>
        </dd>
        <dt>{t("ci.started")}</dt>
        <dd>{j.started_at ? <Time value={j.started_at} /> : "—"}</dd>
        <dt>{t("ci.finished")}</dt>
        <dd>{j.finished_at ? <Time value={j.finished_at} /> : "—"}</dd>
        {j.details && (
          <>
            <dt>{t("ci.reason")}</dt>
            <dd>{j.details}</dd>
          </>
        )}
        <dt>{t("ci.artifacts")}</dt>
        <dd>
          {j.artifact_names.length === 0 ? (
            <Text span size="sm" c="dimmed">
              {t("ci.noArtifacts")}
            </Text>
          ) : (
            <Group gap="sm">
              {j.artifact_names.map((n) => (
                <Anchor key={n} href={api.jobs.artifactUrl(repo, j.sha, n)} download size="sm">
                  {n}
                </Anchor>
              ))}
            </Group>
          )}
        </dd>
      </dl>
      {(retry.isError || cancel.isError) && <ErrorMessage error={retry.error ?? cancel.error} />}
      <LogViewer
        repo={repo}
        attemptId={attemptId}
        action={
          <>
            <Button size="xs" variant="default" onClick={() => retry.mutate()} disabled={retry.isPending}>
              {t("ci.retry")}
            </Button>
            <Button size="xs" variant="default" onClick={() => cancel.mutate()} disabled={cancel.isPending || j.status === "completed"}>
              {t("ci.cancel")}
            </Button>
          </>
        }
      />
    </Stack>
  );
}

/** The drawer of one check: a Branvil job, or a check of the upstream host. */
export function CheckDrawer({ repo, check, onClose }: { repo: string; check: { attemptId: string; hosted: Check | null } | null; onClose: () => void }) {
  const t = useT();
  const [attempt, setAttempt] = useState<string | null>(null);
  const attemptId = attempt ?? check?.attemptId ?? "";
  const job = useJob(repo, attemptId, check !== null && check.hosted === null);
  const hosted = check?.hosted ?? null;
  const close = () => {
    setAttempt(null);
    onClose();
  };
  const title = hosted ? (
    <Group gap="xs">
      {hosted.name}
      <CheckStatusBadge status={hosted.status} conclusion={hosted.conclusion} />
    </Group>
  ) : job.data ? (
    <Group gap="xs">
      {job.data.job}
      <Text span c="dimmed">
        #{job.data.attempt}
      </Text>
      <CheckStatusBadge status={job.data.status} conclusion={job.data.conclusion} />
    </Group>
  ) : (
    t("pull.checks")
  );
  return (
    <Drawer opened={check !== null} onClose={close} position="right" size={780} title={title} closeButtonProps={{ "aria-label": t("common.close") }}>
      {check &&
        (hosted ? (
          <Stack gap="md">
            {hosted.details && (
              <Text size="sm">
                {t("ci.reason")}: {hosted.details}
              </Text>
            )}
            <LogViewer
              repo={repo}
              attemptId={check.attemptId}
              action={
                hosted.details_url && (
                  <Anchor href={hosted.details_url} target="_blank" rel="noopener noreferrer" size="sm">
                    <Group gap={4}>
                      {t("ci.openOnHost")}
                      <ExternalLink size={14} />
                    </Group>
                  </Anchor>
                )
              }
            />
          </Stack>
        ) : (
          <JobDetails repo={repo} attemptId={attemptId} onAttempt={setAttempt} />
        ))}
    </Drawer>
  );
}

/** What a check row asks the drawer to show. */
export const drawerTarget = (c: Check) => ({ attemptId: c.attempt_id ?? "", hosted: isHosted(c) ? c : null });
