import { Card, Group, Stack, Text, Title } from "@mantine/core";
import type { Landing } from "../../api/types";
import { ErrorMessage, Loading, Time } from "../../components/common";
import { useT } from "../../i18n";
import { useLanding, useRepo } from "../../query/hooks";
import { Link } from "../../router/Router";
import { href } from "../../router/routes";
import { LandingReason, QueueLine } from "../pull/LandingStatus";

export function LandingQueue({ repo }: { repo: string }) {
  const t = useT();
  const query = useLanding(repo);
  const repository = useRepo(repo);
  const items = query.data?.items ?? [];
  const active = items.filter((l) => l.state !== "left");
  const left = items.filter((l) => l.state === "left" && l.pull_state === "open" && !l.draft && !["merged", "closed", "draft"].includes(l.reason?.code ?? ""));
  const merged = items.filter((l) => l.state === "left" && l.reason?.code === "merged" && !l.draft);
  const bases = [...new Set(items.map((l) => l.base))];
  const row = (l: Landing) => (
    <Stack key={l.number} gap={6} component="li">
      <Group gap="sm"><Link to={href.pull(repo, l.number)}>!{l.number} {l.title}</Link><Time value={l.state_at} /></Group>
      <LandingReason landing={l} />
    </Stack>
  );
  return (
    <Card p="lg" component="section" aria-label={t("landing.queue")}>
      <Stack gap="md">
        <Title order={3}>{t("landing.queue")}</Title>
        {query.isError || repository.isError ? <ErrorMessage error={query.error ?? repository.error} /> : query.isPending || !repository.data ? <Loading /> : (
          <>
            {active.length === 0 && <Text size="sm" c="dimmed">{t("landing.empty")}</Text>}
            {bases.map((base) => (
              <section key={base} aria-label={base}>
                <Text size="sm" fw={700}>{base}</Text>
                <QueueLine repo={repo} entries={active.filter((l) => l.base === base)} repository={repository.data} />
                {left.some((l) => l.base === base) && <section aria-label={t("landing.left")}><Text fw={600}>{t("landing.left")}</Text><Stack component="ul" gap="md" pl="md">{left.filter((l) => l.base === base).map(row)}</Stack></section>}
                {merged.some((l) => l.base === base) && <section aria-label={t("landing.recent", { base })}><Text fw={600} c="var(--color-success-text)">{t("landing.recent", { base })}</Text><Stack component="ul" gap="sm" pl="md">{merged.filter((l) => l.base === base).sort((a, b) => b.state_at.localeCompare(a.state_at)).slice(0, 3).map(row)}</Stack></section>}
              </section>
            ))}
          </>
        )}
      </Stack>
    </Card>
  );
}
