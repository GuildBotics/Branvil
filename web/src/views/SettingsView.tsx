import { Card, Stack, Text, Title } from "@mantine/core";
import { SquareKanban, Users } from "lucide-react";
import type { ReactNode } from "react";
import { useT } from "../i18n";
import { Link } from "../router/Router";
import { href } from "../router/routes";
import { MembersSettings } from "./settings/MembersSettings";
import { WorkspaceSettings } from "./settings/WorkspaceSettings";

function NavItem({ to, active, icon, children }: { to: string; active: boolean; icon: ReactNode; children: ReactNode }) {
  return (
    <Link to={to} className={active ? "setup-nav-item active" : "setup-nav-item"} aria-current={active ? "page" : undefined}>
      {icon}
      {children}
    </Link>
  );
}

/**
 * Settings, laid out as GuildBotics' setup: sections on the left, one card on
 * the right. The shown workspace (with its repositories) apart from what
 * belongs to all of Branvil (its members).
 */
export function SettingsView({ section, workspace }: { section: string | null; workspace: string | null }) {
  const t = useT();
  const current = section === "members" || !workspace ? "members" : "workspace";
  return (
    <Stack gap="lg">
      <Title order={2}>{t("nav.settings")}</Title>
      <div className="setup-layout">
        <Card p="xs">
          <nav className="setup-nav" aria-label={t("settings.sections")}>
            {workspace && (
              <div className="setup-nav-group">
                <div className="nav-label">{t("settings.workspaceGroup")}</div>
                <NavItem to={href.settings()} active={current === "workspace"} icon={<SquareKanban size={18} />}>
                  {workspace}
                </NavItem>
              </div>
            )}
            <div className="setup-nav-group">
              <div className="nav-label">{t("settings.branvilGroup")}</div>
              <NavItem to={href.settings("members")} active={current === "members"} icon={<Users size={18} />}>
                {t("settings.members")}
              </NavItem>
            </div>
          </nav>
        </Card>
        <div>
          {current === "workspace" && workspace ? <WorkspaceSettings workspace={workspace} /> : <MembersSettings />}
          {!workspace && (
            <Text size="sm" c="dimmed" mt="sm">
              {t("workspace.noneHelp")}
            </Text>
          )}
        </div>
      </div>
    </Stack>
  );
}
