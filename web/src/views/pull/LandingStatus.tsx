import { Group, Stack, Text } from "@mantine/core";
import type { Landing, LandingFirst, PullRequest } from "../../api/types";
import { Sha, StateBadge, Time } from "../../components/common";
import { useT } from "../../i18n";
import { useChecks } from "../../query/hooks";
import { Link } from "../../router/Router";
import { href } from "../../router/routes";
import { RunTime } from "./Checks";

function useLandingLabel() {
  const t = useT();
  return (state: Landing["state"], error: string | null = null, host = true, published = false) =>
    state === "passed" ? t(error ? "landing.retrying" : !host ? "landing.passed" : published ? "landing.hostWaiting" : "landing.publishWaiting") : t(`landing.${state}`);
}

export function LandingBadge({ landing, host = true }: { landing: Landing; host?: boolean }) {
  const t = useT();
  const label = useLandingLabel();
  const error = landing.error;
  return (
    <StateBadge tone={landing.state === "left" ? "danger" : landing.state === "passed" && !error ? "success" : "warning"}>
      {landing.state === "waiting" ? t("landing.position", { position: landing.position ?? "—" }) : label(landing.state, error, host, landing.published)}
    </StateBadge>
  );
}

/** The queue leader explains why every following pull request is waiting. */
export function LandingLeader({ repo, first, host = true }: { repo: string; first: LandingFirst; host?: boolean }) {
  const t = useT();
  const label = useLandingLabel();
  return (
    <Stack gap={4}>
      <Text size="sm">
        {t("landing.first")} <Link to={href.pull(repo, first.number)}>!{first.number}</Link> · {label(first.state, first.error, host, first.published)}
      </Text>
      <Text size="xs" c="dimmed">
        <Time value={first.state_at} /> · <RunTime startedAt={first.state_at} />
      </Text>
      {first.error && <Text size="sm" c="var(--color-warning-text)">{first.error}</Text>}
    </Stack>
  );
}

export function LandingReason({ landing }: { landing: Landing }) {
  const t = useT();
  const reason = landing.reason;
  if (!reason) return null;
  const recovery = reason.code === "gate_failed" ? "landing.retryGate" : reason.code === "host_failed" ? "landing.retryHost" : reason.code === "publish_stopped" ? "landing.publishAgain" : "landing.rejoin";
  return (
    <Stack gap={4}>
      <Text size="sm" c="var(--color-danger-text)">{reason.message}</Text>
      {reason.jobs.length > 0 && <Text size="sm">{t("landing.failedJobs", { jobs: reason.jobs.join(", ") })}</Text>}
      {reason.files.length > 0 && <Text size="sm">{t("landing.conflicts", { files: reason.files.join(", ") })}</Text>}
      {reason.code !== "merged" && <Text size="sm" c="dimmed">{t(recovery)}</Text>}
    </Stack>
  );
}

/** The third landing stage, after checks on the head and approvals on that head. */
export function GateStage({ pull, host }: { pull: PullRequest; host: boolean }) {
  const t = useT();
  const landing = pull.landing;
  const checks = useChecks(pull.repository, pull.number, landing?.state === "gating" || landing?.state === "left");
  const gateChecks = checks.data?.checks.filter((c) => c.name.startsWith("gate/")) ?? [];
  return (
    <Stack gap={6} component="section" aria-label={t("landing.gate")}>
      <Text size="sm" fw={700}>{t("landing.gate")}</Text>
      {!landing ? <Text size="sm" c="dimmed">{t("landing.notQueued")}</Text> : (
        <>
          <Group gap="xs"><LandingBadge landing={landing} host={host} />{landing.gate && <Link to={href.commit(pull.repository, landing.gate)}><Sha value={landing.gate} /></Link>}</Group>
          {landing.state === "waiting" && landing.first ? <LandingLeader repo={pull.repository} first={landing.first} host={host} /> : (
            <Text size="xs" c="dimmed"><Time value={landing.state_at} />{landing.state !== "left" && <> · <RunTime startedAt={landing.state_at} /></>}</Text>
          )}
          {landing.state === "passed" && landing.error && <Text size="sm" c="var(--color-warning-text)">{landing.error}</Text>}
          {gateChecks.map((c) => c.attempt_id ? <Link key={c.name} to={href.job(pull.repository, c.attempt_id)}>{c.name}</Link> : <Text key={c.name} size="sm">{c.name}</Text>)}
          {landing.state === "left" && <LandingReason landing={landing} />}
        </>
      )}
    </Stack>
  );
}

export function gatePassed(pull: PullRequest): boolean {
  const l = pull.landing;
  return l?.state === "passed" && l.position === 1 && l.base_sha === pull.base_sha && l.head_sha === pull.head_sha;
}
