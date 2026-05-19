import type { ServerSettings } from "@qltysh/fabro-api-client";
import { useServerSettings } from "../lib/queries";
import {
  Panel,
  PanelSkeleton,
  Row,
  SettingsPageIntro,
  Toggle,
} from "../components/settings-panel";

export function meta() {
  return [{ title: "Integrations — Fabro" }];
}

const DESCRIPTION = (
  <>
    External services connected to this server. Edit via{" "}
    <code className="font-mono text-fg-2">settings.toml</code>; changes take
    effect on the next server restart.
  </>
);

export default function SettingsIntegrations() {
  const settingsQuery = useServerSettings();
  const settings = settingsQuery.data;

  return (
    <div className="space-y-6">
      <SettingsPageIntro description={DESCRIPTION} />
      {settings ? <IntegrationsPanel settings={settings} /> : <PanelSkeleton />}
    </div>
  );
}

function IntegrationsPanel({ settings }: { settings: ServerSettings }) {
  const { integrations } = settings.server;
  return (
    <Panel title="Integrations">
      <Row title="GitHub" help="App for repo access, checks, and PR automation.">
        <IntegrationValue
          enabled={integrations.github.enabled}
          detail={
            integrations.github.slug
              ? `app: ${integrations.github.slug}`
              : integrations.github.app_id
                ? `app id: ${integrations.github.app_id}`
                : undefined
          }
        />
      </Row>
    </Panel>
  );
}

function IntegrationValue({
  enabled,
  detail,
}: {
  enabled: boolean;
  detail?: string;
}) {
  if (!enabled) return <Toggle on={false} />;
  return (
    <span className="inline-flex flex-wrap items-center gap-x-2 gap-y-1">
      <Toggle on={true} />
      {detail ? (
        <span className="font-mono text-xs text-fg-3">{detail}</span>
      ) : null}
    </span>
  );
}
