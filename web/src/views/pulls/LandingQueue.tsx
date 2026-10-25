import { Card, Group, Stack, Text, Title } from "@mantine/core";
import type { Landing } from "../../api/types";
import { ErrorMessage, Loading, Time } from "../../components/common";
import { useT } from "../../i18n";
import { useLanding, useRepo } from "../../query/hooks";
import { Link } from "../../router/Router";
import { href } from "../../router/routes";
import { LandingBadge, LandingLeader, LandingReason } from "../pull/LandingStatus";

export function LandingQueue({ repo }: { repo: string }) {
  const t = useT();
  const query = useLanding(repo);
  const repository = useRepo(repo);
  const host = repository.data?.upstream != null;
  const active = query.data?.items.filter((l) => l.state !== "left") ?? [];
  const left = query.data?.items.filter((l) => l.state === "left") ?? [];
  const bases = [...new Set(active.map((l) => l.base))];
  const row = (l: Landing) => (
    <Stack key={l.number} gap={6} component="li">
      <Group gap="sm">
        <Link to={href.pull(repo, l.number)}>!{l.number}</Link>
        <LandingBadge landing={l} host={host} />
        <Text size="xs" c="dimmed">{t("landing.queuedAt")} <Time value={l.queued_at} /></Text>
      </Group>
      {l.position === 1 && l.first && <LandingLeader repo={repo} first={l.first} host={host} />}
      {l.state === "left" && <LandingReason landing={l} />}
    </Stack>
  );
  return (
    <Card p="lg" component="section" aria-label={t("landing.queue")}>
      <Stack gap="md">
        <Title order={3}>{t("landing.queue")}</Title>
        {query.isError ? <ErrorMessage error={query.error} /> : query.isPending ? <Loading /> : (
          <>
            {active.length === 0 && <Text size="sm" c="dimmed">{t("landing.empty")}</Text>}
            {bases.map((base) => (
              <section key={base} aria-label={base}>
                <Text size="sm" fw={700}>{base}</Text>
                <Stack component="ol" gap="md" pl="md" my="xs">
                  {active.filter((l) => l.base === base).sort((a, b) => (a.position ?? 0) - (b.position ?? 0)).map(row)}
                </Stack>
              </section>
            ))}
            {left.length > 0 && (
              <section aria-label={t("landing.left")}>
                <Text size="sm" fw={700}>{t("landing.left")}</Text>
                <Stack component="ul" gap="md" pl="md" my="xs">{left.map(row)}</Stack>
              </section>
            )}
          </>
        )}
      </Stack>
    </Card>
  );
}
