import { Anchor, Badge, Card, Group, Progress, Stack, Text } from "@mantine/core";
import { Check, Cloud, GitBranch, GitMerge, Hourglass, ListChecks, ShieldCheck, X, type LucideIcon } from "lucide-react";
import type { Landing, LandingFirst, LandingNext, PullRequest, Repository } from "../../api/types";
import { StateBadge, type Tone } from "../../components/common";
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
      <Elapsed first={first} />
    </Stack>
  );
}

function Elapsed({ first }: { first: LandingFirst }) {
  const t = useT();
  return (
    <Text size="xs" c="dimmed">
      {t("landing.elapsed")}{" "}
      <RunTime startedAt={first.state_at} />
    </Text>
  );
}

const REASONS = ["merged", "closed", "draft", "conflict", "gate_failed", "host_failed", "publish_stopped", "already_in_base", "head_moved", "base_moved", "empty_head", "publish_requirements", "head_checks", "approvals"] as const;
const reasonKey = (code: string): MessageKey => REASONS.includes(code as typeof REASONS[number]) ? `landing.reason.${code}` as MessageKey : "landing.reason.unknown";

export function LandingReason({ landing }: { landing: Landing }) {
  const t = useT();
  const reason = landing.reason;
  if (!reason) return null;
  const tone = reasonTone(reason.code);
  return (
    <Stack gap={4} style={{ overflowWrap: "anywhere" }}>
      <Text size="sm" c={tone === "neutral" ? "dimmed" : `var(--color-${tone}-text)`}>{t(reasonKey(reason.code))}</Text>
      <ReasonDetail landing={landing} />
    </Stack>
  );
}

/** What the departure left behind and how to rejoin, under its reason. */
function ReasonDetail({ landing }: { landing: Landing }) {
  const t = useT();
  const reason = landing.reason;
  if (!reason) return null;
  const recovery = reason.code === "gate_failed" ? "landing.retryGate" : reason.code === "host_failed" ? "landing.retryHost" : reason.code === "publish_stopped" ? "landing.publishAgain" : ["already_in_base", "empty_head"].includes(reason.code) ? "landing.closePull" : moved(reason.code) ? "landing.waitRejoin" : "landing.rejoin";
  return (
    <>
      {reason.code === "publish_stopped" && <Text size="sm">{reason.message}</Text>}
      {reason.jobs.length > 0 && <Text size="sm">{t("landing.failedJobs", { jobs: reason.jobs.join(", ") })}</Text>}
      {reason.files.length > 0 && <Text size="sm">{t("landing.conflicts", { files: reason.files.join(", ") })}</Text>}
      {reason.code !== "merged" && <Text size="sm" c="dimmed">{t(recovery)}</Text>}
    </>
  );
}

/** Required gate jobs completed on the leader's head. */
function useGateDone(repo: string, leader: { number: number; head_sha: string }, required: string[]): number {
  const checks = useChecks(repo, leader.number, true);
  return required.filter((name) => checks.data?.head_sha === leader.head_sha && checks.data.checks.some((c) => c.name === `gate/${name}` && c.status === "completed")).length;
}

function GateProgress({ repo, leader, required }: { repo: string; leader: Landing; required: string[] }) {
  return <Text size="xs">{useGateDone(repo, leader, required)}/{required.length}</Text>;
}

/** The repository page's queue: who waits, where the leader is, and the way to the base. */
export function QueueLine({ repo, entries, repository }: { repo: string; entries: Landing[]; repository: Repository }) {
  const t = useT();
  const active = entries.filter((e) => e.state !== "left").sort((a, b) => (a.position ?? 0) - (b.position ?? 0));
  const leader = active.find((entry) => entry.position === 1);
  if (!leader?.first || leader.first.number !== leader.number) return null;
  const first = leader.first;
  const host = repository.upstream?.host ?? "";
  const waiting = active.slice(1);
  const shown = waiting.slice(0, 3).sort((a, b) => (b.position ?? 0) - (a.position ?? 0));
  const place = first.next === "gate" ? "gate" : host && !["publishing", "owner_publish"].includes(first.next) ? "host" : "transit";
  const node = (entry: Landing) => (
    <div className="landing-pr" data-pr={entry.number}>
      <span className="landing-dot">!{entry.number}</span>
      <Text size="sm" fw={600}>{entry.title}</Text>
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

export type CheckState = "checking" | "check.missing" | "check.running" | "check.pathsSkipped" | "check.success" | "check.failure";
const satisfied = (state: CheckState) => state === "check.success" || state === "check.pathsSkipped";

/** Each required check on this head, as check_landable reads it: real success or a service path selection. */
export function useRequiredChecks(pull: PullRequest, required: string[]): { name: string; state: CheckState }[] {
  const query = useChecks(pull.repository, pull.number, required.length > 0);
  const current = !query.isError && query.data?.head_sha === pull.head_sha ? query.data : undefined;
  return required.map((name) => {
    const c = current?.checks.find((check) => check.name === name);
    const state: CheckState = !current ? "checking" : !c ? "check.missing" : c.status !== "completed" ? "check.running" : c.conclusion === "success" ? "check.success" : c.conclusion === "skipped" && c.path_skip ? "check.pathsSkipped" : "check.failure";
    return { name, state };
  });
}

export const useMissingChecks = (pull: PullRequest, required: string[]): string[] =>
  useRequiredChecks(pull, required).filter((c) => !satisfied(c.state)).map((c) => c.name);

/** The stops on a pull request's way into its base; the host's only with an upstream. */
export type Stop = "conditions" | "queue" | "gate" | "host" | "merge" | "base";
/** At the current stop: Branvil (or the host) is working, a person has a turn, or the pull request left the queue there. */
export type Mark = "now" | "turn" | "failed";

const NEXT_STOP: Record<LandingNext, Stop> = {
  gate: "gate", publishing: "host", owner_publish: "host", host_checks: "host", host_closed: "host", owner_fix_auto: "host",
  owner_merge_host: "merge", auto_merge: "merge", owner_merge: "merge", following: "base", owner_fix_following: "base",
};
const REASON_STOP: Partial<Record<string, Stop>> = { conflict: "gate", gate_failed: "gate", already_in_base: "gate", host_failed: "host", publish_stopped: "host" };

/**
 * Where the pull request is on its way into the base. A queue entry for an
 * older head, or one that left for a reason the head itself clears (a move, a
 * draft), means the pull request is collecting what it needs to join again.
 */
export function whereabouts(pull: PullRequest, repo: Repository, personNeeded: boolean): { stops: Stop[]; at: Stop; mark: Mark; landing: Landing | null } {
  const stops: Stop[] = repo.upstream ? ["conditions", "queue", "gate", "host", "merge", "base"] : ["conditions", "queue", "gate", "merge", "base"];
  const l = pull.landing?.head_sha === pull.head_sha ? pull.landing : null;
  if (!l || (l.state === "left" && reasonTone(l.reason?.code) === "neutral")) return { stops, at: "conditions", mark: personNeeded ? "turn" : "now", landing: null };
  if (l.state === "left") return { stops, at: REASON_STOP[l.reason?.code ?? ""] ?? "conditions", mark: "failed", landing: l };
  if (l.position !== 1 || !l.first) return { stops, at: "queue", mark: "now", landing: l };
  const at = NEXT_STOP[l.first.next];
  return { stops, at: at === "host" && !repo.upstream ? "merge" : at, mark: humanTurn(l.first) ? "turn" : "now", landing: l };
}

const ICONS: Record<Stop, LucideIcon> = { conditions: ListChecks, queue: Hourglass, gate: ShieldCheck, host: Cloud, merge: GitMerge, base: GitBranch };
const MARK_COLOR: Record<Mark, string> = { now: "var(--color-brand)", turn: "var(--color-warning-text)", failed: "var(--color-danger-text)" };

/** The pull request page's answer to "where is this pull request, and whose turn is it". */
export function LandingJourney({ pull, repo }: { pull: PullRequest; repo: Repository }) {
  const t = useT();
  const checks = useRequiredChecks(pull, repo.required_checks);
  const queue = useLanding(pull.repository);
  // What the head still lacks to join; a person must act on all but a check CI is still producing.
  const unmet = [
    ...(pull.draft ? [{ text: t("pull.draft"), person: true }] : []),
    ...checks.filter((c) => !satisfied(c.state)).map((c) => ({ text: `${c.name} · ${t(`landing.${c.state}`)}`, person: c.state === "check.failure" })),
    ...(pull.approvals < repo.required_approvals ? [{ text: t("merge.approvals", { count: pull.approvals, required: repo.required_approvals }), person: true }] : []),
  ];
  const { stops, at, mark, landing } = whereabouts(pull, repo, unmet.some((u) => u.person));
  const host = repo.upstream?.host ?? "";
  const first = landing?.position === 1 ? landing.first : null;
  const behind = first ? (queue.data?.items.filter((e) => e.base === pull.base && e.state !== "left" && e.number !== pull.number).length ?? 0) : 0;
  const headline = mark === "failed" ? t("landing.leftWith", { reason: t(reasonKey(landing?.reason?.code ?? "")) })
    : at === "conditions" ? t(unmet.length ? "landing.missing" : "landing.notInQueue")
    : first ? t(`landing.next.${first.next}`, { host, base: pull.base })
    : t("landing.waitingAt", { position: landing?.position ?? "—" });
  const name = (stop: Stop) => stop === "host" ? host : stop === "base" ? pull.base : stop === "gate" ? t("landing.checkpoint") : t(`landing.stop.${stop}`);
  const reached = stops.indexOf(at);
  const Icon = mark === "failed" ? X : ICONS[at];
  return (
    <Card p="lg" component="section" aria-label={t("landing.journey", { base: pull.base })}>
      <Stack gap="md">
        <Group gap="xs" wrap="nowrap">
          <Icon size={18} color={MARK_COLOR[mark]} aria-hidden="true" />
          <Text fw={700} c={mark === "now" ? undefined : MARK_COLOR[mark]}>{headline}</Text>
        </Group>
        <ol className="journey">
          {stops.map((stop, i) => {
            const state = i < reached ? "done" : i === reached ? mark : "todo";
            const StopIcon = state === "done" ? Check : state === "failed" ? X : ICONS[stop];
            return (
              <li key={stop} className="journey-stop" data-stop={stop} data-state={state} aria-current={i === reached ? "step" : undefined}>
                <span className="journey-dot" aria-hidden="true">{stop === "queue" && i === reached ? landing?.position : <StopIcon size={14} />}</span>
                <span>{name(stop)}</span>
              </li>
            );
          })}
        </ol>
        <Stack gap={6} style={{ overflowWrap: "anywhere" }}>
          {at === "conditions" && unmet.length > 0 && (
            <Group gap={6}>
              {unmet.map((u) => (
                <Badge key={u.text} color={u.person ? "warning" : "neutral"} variant="light" size="sm" h="auto" styles={{ label: { whiteSpace: "normal", overflowWrap: "anywhere" } }}>
                  {u.text}
                </Badge>
              ))}
            </Group>
          )}
          {mark === "failed" && landing ? (
            <ReasonDetail landing={landing} />
          ) : at === "conditions" ? (
            <Text size="sm" c="dimmed">{t(unmet.length ? "landing.notQueued" : "landing.headReady")}</Text>
          ) : !first ? (
            landing?.first && <Text size="sm" c="dimmed">{t("landing.waitFor", { first: landing.first.number })}</Text>
          ) : (
            <>
              {first.next === "gate" && <GateBar pull={pull} required={repo.required_checks} />}
              {first.next_detail && <Text size="sm">{first.next_detail}</Text>}
              {["owner_merge_host", "host_closed"].includes(first.next) && first.host_url && (
                <Anchor href={first.host_url} target="_blank" rel="noopener noreferrer" size="sm">{t("landing.action")}</Anchor>
              )}
              {behind > 0 && <Text size="sm" c="dimmed">{t("landing.behind", { count: behind })}</Text>}
              <Elapsed first={first} />
            </>
          )}
        </Stack>
      </Stack>
    </Card>
  );
}

function GateBar({ pull, required }: { pull: PullRequest; required: string[] }) {
  const done = useGateDone(pull.repository, pull, required);
  return (
    <Group gap="xs" wrap="nowrap">
      <Progress value={required.length ? (done / required.length) * 100 : 0} size="sm" w={120} aria-hidden="true" />
      <Text size="sm">{done}/{required.length}</Text>
    </Group>
  );
}

export function gatePassed(pull: PullRequest): boolean {
  const l = pull.landing;
  return l?.state === "passed" && l.position === 1 && l.base_sha === pull.base_sha && l.head_sha === pull.head_sha;
}
