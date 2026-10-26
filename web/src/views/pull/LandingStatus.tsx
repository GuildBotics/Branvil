import { Anchor, Group, Stack, Text } from "@mantine/core";
import { GitBranch } from "lucide-react";
import type { Landing, LandingFirst, PullRequest, Repository } from "../../api/types";
import { ErrorMessage, Loading, Sha, StateBadge, type Tone } from "../../components/common";
import { useT, type MessageKey } from "../../i18n";
import { useChecks, useLanding } from "../../query/hooks";
import { Link } from "../../router/Router";
import { href } from "../../router/routes";
import { RunTime } from "./Checks";

const humanTurn = (first: LandingFirst) => first.next.startsWith("owner_") || first.next === "host_closed";
const moved = (code: string) => ["head_moved", "base_moved"].includes(code);
const reasonTone = (code?: string): Tone => code === "merged" ? "success" : !code || moved(code) || ["closed", "draft"].includes(code) ? "neutral" : "warning";

export function LandingBadge({ landing, host = "" }: { landing: Landing; host?: string }) {
  const t = useT();
  const first = landing.position === 1 ? landing.first : null;
  return (
    <StateBadge tone={landing.state === "left" ? reasonTone(landing.reason?.code) : first && humanTurn(first) ? "warning" : "neutral"}>
      {first ? t(`landing.badge.${first.next}`, { host, base: landing.base }) : landing.state === "waiting" ? t("landing.position", { position: landing.position ?? "—" }) : t(`landing.${landing.state}`)}
    </StateBadge>
  );
}

/** Decisions belong to the service. Links appear only when the owner has a turn. */
export function LandingLeader({ repo, first, host, base }: { repo: string; first: LandingFirst; host: string; base: string }) {
  const t = useT();
  const label = t(`landing.next.${first.next}`, { host, base });
  const human = humanTurn(first);
  const atHost = first.next === "owner_merge_host" || first.next === "host_closed";
  return (
    <Stack gap={4} style={{ overflowWrap: "anywhere" }}>
      <Text size="sm" c={human ? "var(--color-warning-text)" : undefined}>{label}</Text>
      {first.next_detail && <Text size="sm">{first.next_detail}</Text>}
      {human && (atHost && first.host_url ? (
        <Anchor href={first.host_url} target="_blank" rel="noopener noreferrer" size="sm">{t("landing.action")}</Anchor>
      ) : <Link to={href.pull(repo, first.number)}>{t("landing.action")}</Link>)}
      <Text size="xs" c="dimmed">
        {t("landing.elapsed")}{" "}
        <RunTime startedAt={first.state_at} />
      </Text>
    </Stack>
  );
}

const REASONS = ["merged", "closed", "draft", "conflict", "gate_failed", "host_failed", "publish_stopped", "already_in_base", "head_moved", "base_moved", "empty_head", "publish_requirements", "head_checks", "approvals"] as const;
export function LandingReason({ landing }: { landing: Landing }) {
  const t = useT();
  const reason = landing.reason;
  if (!reason) return null;
  const tone = reasonTone(reason.code);
  const key = REASONS.includes(reason.code as typeof REASONS[number]) ? `landing.reason.${reason.code}` as MessageKey : "landing.reason.unknown";
  const recovery = reason.code === "gate_failed" ? "landing.retryGate" : reason.code === "host_failed" ? "landing.retryHost" : reason.code === "publish_stopped" ? "landing.publishAgain" : ["already_in_base", "empty_head"].includes(reason.code) ? "landing.closePull" : moved(reason.code) ? "landing.waitRejoin" : "landing.rejoin";
  return (
    <Stack gap={4} style={{ overflowWrap: "anywhere" }}>
      <Text size="sm" c={tone === "neutral" ? "dimmed" : `var(--color-${tone}-text)`}>{t(key)}</Text>
      {reason.code === "publish_stopped" && <Text size="sm">{reason.message}</Text>}
      {reason.jobs.length > 0 && <Text size="sm">{t("landing.failedJobs", { jobs: reason.jobs.join(", ") })}</Text>}
      {reason.files.length > 0 && <Text size="sm">{t("landing.conflicts", { files: reason.files.join(", ") })}</Text>}
      {reason.code !== "merged" && <Text size="sm" c="dimmed">{t(recovery)}</Text>}
    </Stack>
  );
}

function GateProgress({ repo, leader, required }: { repo: string; leader: Landing; required: string[] }) {
  const checks = useChecks(repo, leader.number, true);
  const completed = required.filter((name) => checks.data?.head_sha === leader.head_sha && checks.data.checks.some((c) => c.name === `gate/${name}` && c.status === "completed")).length;
  return <Text size="xs">{completed}/{required.length}</Text>;
}

/** The repository and PR pages share the same path and leader placement. */
export function QueueLine({ repo, entries, repository, highlight }: { repo: string; entries: Landing[]; repository: Repository; highlight?: number }) {
  const t = useT();
  const active = entries.filter((e) => e.state !== "left").sort((a, b) => (a.position ?? 0) - (b.position ?? 0));
  const leader = active.find((entry) => entry.position === 1);
  if (!leader?.first || leader.first.number !== leader.number) return null;
  const first = leader.first;
  const host = repository.upstream?.host ?? "";
  const waiting = active.slice(1);
  const shown = waiting.slice(0, 3);
  const selected = waiting.find((entry) => entry.number === highlight);
  if (selected && !shown.includes(selected)) shown[2] = selected;
  shown.sort((a, b) => (b.position ?? 0) - (a.position ?? 0));
  const place = first.next === "gate" ? "gate" : host && !["publishing", "owner_publish"].includes(first.next) ? "host" : "transit";
  const node = (entry: Landing) => (
    <div className={`landing-pr${entry.number === highlight ? " landing-pr-highlight" : ""}`} data-pr={entry.number}>
      <span className="landing-dot">!{entry.number}</span>
      {(highlight == null || entry.number === highlight) && <Text size="sm" fw={600}>{entry.title}</Text>}
    </div>
  );
  const leaderAt = (slot: string) => place === slot ? <LandingLeader repo={repo} first={first} host={host} base={leader.base} /> : null;
  const marker = (slot: string, icon: React.ReactNode) => <div className="landing-marker">{icon}{place === slot && <div className="landing-marker-pr">{node(leader)}</div>}</div>;
  return (
    <div className="landing-line" aria-label={leader.base}>
      <div className="landing-stop landing-waiting" data-stage="waiting">
        <div className="landing-waiters">{shown.map((e) => <div key={e.number}>{node(e)}</div>)}</div>
        {waiting.length > 3 && <Text size="sm">{t("landing.others", { count: waiting.length - 3 })}</Text>}
        {waiting.length > 0 && <Text size="xs" c="dimmed">{t("landing.waitFor", { first: first.number })}</Text>}
      </div>
      <div className="landing-stop" data-stage="gate">
        {marker("gate", <span className="landing-square" aria-hidden="true">✓</span>)}
        <Text size="sm" fw={600}>{t("landing.checkpoint")}</Text>
        {first.next === "gate" && <GateProgress repo={repo} leader={leader} required={repository.required_checks} />}
        {leaderAt("gate")}
      </div>
      <div className="landing-stop" data-stage="transit">
        <div className="landing-marker">{place === "transit" && node(leader)}</div>
        {!host && <Text size="sm">{t("landing.passedPoint")}</Text>}
        {leaderAt("transit")}
      </div>
      {host && <div className="landing-stop" data-stage="host">{marker("host", <span className="landing-place" aria-hidden="true">☁</span>)}<Text size="sm" fw={600}>{host}</Text>{leaderAt("host")}</div>}
      <div className="landing-stop" data-stage="base">
        <div className="landing-marker"><span className="landing-square" aria-hidden="true"><GitBranch size={16} /></span></div>
        <Text size="sm" fw={600}>{leader.base}</Text>
      </div>
    </div>
  );
}

/** The same queue path, emphasizing the pull request being read. */
export function GateStage({ pull, repository }: { pull: PullRequest; repository: Repository }) {
  const t = useT();
  const landing = pull.landing;
  const queue = useLanding(pull.repository);
  const checks = useChecks(pull.repository, pull.number, landing?.state === "gating" || landing?.state === "left");
  const gateChecks = checks.data?.checks.filter((c) => c.name.startsWith("gate/")) ?? [];
  const entries = queue.data?.items.filter((e) => e.base === pull.base && e.number !== pull.number) ?? [];
  if (landing) entries.push(landing);
  return (
    <Stack gap={6} component="section" aria-label={t("landing.gate")} style={{ overflowWrap: "anywhere" }}>
      <Text size="sm" fw={700}>{t("landing.gate")}</Text>
      {!landing ? <Text size="sm" c="dimmed">{t("landing.notQueued")}</Text> : (
        <>
          <Group gap="xs"><LandingBadge landing={landing} host={repository.upstream?.host} />{landing.gate && <Sha repo={pull.repository} value={landing.gate} />}</Group>
          {queue.isError ? <ErrorMessage error={queue.error} /> : queue.isPending ? <Loading /> : <QueueLine repo={pull.repository} entries={entries} repository={repository} highlight={pull.number} />}
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
