"use client";

import { Check, ExternalLink, Loader2 } from "lucide-react";

import { cn } from "@/lib/utils";
import type {
  CapabilityReport,
  LifecycleState,
  PluginDependency,
  PluginType,
  PluginSections,
  ReleaseEntry,
  SecretDefinition,
} from "@/types/plugins";
import {
  PluginCapabilities,
  visibleCapabilities,
} from "@/components/plugin-capabilities";
import { ReleaseStateBadge } from "@/components/plugin-lifecycle";
import { PublishedMarkdown } from "@/components/published-markdown";
import { UsageBlock } from "@/components/usage-block";
import {
  PluginBanner,
  PluginMark,
  TYPE_LABEL,
  TYPE_NOTE,
} from "@/components/plugin-identity";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Separator } from "@/components/ui/separator";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";

/**
 * One plugin, however it was reached. An installed plugin and a catalogue
 * entry differ in what you can do with them, not in what they are, so both
 * routes build this and the page decides nothing about where its data came
 * from.
 */
export type PluginView = {
  id: string;
  name: string;
  version: string;
  description: string | null;
  iconUrl: string | null;
  bannerUrl: string | null;
  type: PluginType;
  namespace: string | null;
  capabilities: CapabilityReport;
  dependencies: PluginDependency[];
  allowedHosts: string[];
  requiredSecrets: SecretDefinition[];
  sha256: string | null;
  /** Where the module came from. Absent for a catalogue entry. */
  source: { kind: "file" | "url"; url: string | null } | null;
  installed: boolean;
  /** Set when installed and the catalogue knows a newer release. */
  updateAvailable: boolean;
  latestVersion: string | null;
  /** Every release the catalogue knows, newest first. */
  releases: ReleaseEntry[];
  /** The registry project behind the plugin, and where this version stands
   * in it. Absent for a plugin the registry knows nothing about. */
  registry: {
    did: string | null;
    /** The channel followed, or offered; `null` when pinned. */
    channel: string | null;
    state: LifecycleState | null;
    reason: string | null;
    /** A URL install no registry project matched. */
    unresolved: boolean;
  } | null;
  /** The publisher's long-form prose, where the catalogue has any. */
  sections: PluginSections;
};

function Fact({
  label,
  children,
}: {
  label: string;
  children: React.ReactNode;
}) {
  return (
    <div className="flex flex-col gap-1">
      <dt className="text-xs text-muted-foreground">{label}</dt>
      <dd className="text-sm break-words">{children}</dd>
    </div>
  );
}

export function PluginDetail({
  plugin,
  actions,
  settings,
}: {
  plugin: PluginView;
  /** Install, or the controls an installed plugin has. */
  actions: React.ReactNode;
  /** Secrets and hosts, rendered only where they can be saved. */
  settings?: React.ReactNode;
}) {
  const capabilities = visibleCapabilities(plugin.capabilities);

  return (
    <div className="flex flex-col gap-6">
      <PluginBanner bannerUrl={plugin.bannerUrl} />

      <div className="flex flex-wrap items-start gap-4">
        <PluginMark
          name={plugin.name}
          type={plugin.type}
          iconUrl={plugin.iconUrl}
          size="sheet"
        />
        {/* A basis wide enough that the actions wrap below rather than
            squeezing the name into a one-word column on a phone. */}
        <div className="flex min-w-0 flex-1 basis-64 flex-col gap-2">
          <div className="flex flex-wrap items-center gap-2">
            <h2 className="text-xl font-semibold">{plugin.name}</h2>
            <Badge variant="outline" className="font-mono tabular-nums">
              {plugin.version}
            </Badge>
            {plugin.updateAvailable && plugin.latestVersion ? (
              <Badge variant="destructive" className="font-mono tabular-nums">
                {plugin.latestVersion} available
              </Badge>
            ) : null}
          </div>
          <div className="flex flex-wrap items-center gap-2 text-sm text-muted-foreground">
            <span>{TYPE_LABEL[plugin.type]}</span>
            <span aria-hidden>·</span>
            <span>{TYPE_NOTE[plugin.type]}</span>
          </div>
          <p className="max-w-prose text-sm">
            {plugin.description ?? "No description."}
          </p>
        </div>
        <div className="flex flex-wrap items-center gap-2">{actions}</div>
      </div>

      {plugin.namespace ? <UsageBlock namespace={plugin.namespace} /> : null}

      <Separator />

      <Tabs defaultValue="overview">
        <TabsList>
          <TabsTrigger value="overview">Overview</TabsTrigger>
          <TabsTrigger value="versions">
            Versions
            {plugin.releases.length > 0 ? (
              <span className="ml-1 text-muted-foreground tabular-nums">
                {plugin.releases.length}
              </span>
            ) : null}
          </TabsTrigger>
          <TabsTrigger value="permissions">Permissions</TabsTrigger>
          {settings ? (
            <TabsTrigger value="settings">Settings</TabsTrigger>
          ) : null}
        </TabsList>

        <TabsContent value="overview" className="flex flex-col gap-6 pt-4">
          <dl className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
            <Fact label="Identifier">
              <span className="font-mono text-xs">{plugin.id}</span>
            </Fact>
            {plugin.namespace ? (
              <Fact label="Namespace">
                <span className="font-mono text-xs">{plugin.namespace}</span>
              </Fact>
            ) : null}
            <Fact label="Kind">{TYPE_LABEL[plugin.type]}</Fact>
            {/* Where the module came from is a fact about an installation.
                Before one, the URL under Permissions is the honest version. */}
            {plugin.installed && plugin.source ? (
              <Fact label="Source">
                {plugin.registry?.did
                  ? "Installed from the registry"
                  : plugin.source.kind === "url"
                    ? "Installed from URL"
                    : "Local file"}
              </Fact>
            ) : null}
            {plugin.registry?.did ? (
              <>
                <Fact label="Publisher">
                  <span className="font-mono text-xs">
                    {plugin.registry.did}
                  </span>
                </Fact>
                {plugin.installed ? (
                  <Fact label="Channel">
                    {plugin.registry.channel ?? "Pinned to this version"}
                  </Fact>
                ) : null}
                <Fact label="Version state">
                  {plugin.registry.state ? (
                    <span className="flex flex-col items-start gap-1">
                      {plugin.registry.state === "active" ? (
                        "Active"
                      ) : (
                        <ReleaseStateBadge state={plugin.registry.state} />
                      )}
                      {plugin.registry.reason ? (
                        <span className="text-muted-foreground">
                          {plugin.registry.reason}
                        </span>
                      ) : null}
                    </span>
                  ) : (
                    <span className="text-muted-foreground">
                      Not in the catalogue
                    </span>
                  )}
                </Fact>
              </>
            ) : null}
            {plugin.registry?.unresolved ? (
              <Fact label="Registry project">
                <span className="text-muted-foreground">Unresolved</span>
              </Fact>
            ) : null}
            {plugin.allowedHosts.length > 0 ? (
              <Fact label="Reaches">
                <span className="font-mono text-xs">
                  {plugin.allowedHosts.join(", ")}
                </span>
              </Fact>
            ) : null}
          </dl>

          {/* Named, not filled in: the Settings tab is where values go, and
              when there is no Settings tab this is the only place an operator
              learns what the plugin will ask for. */}
          {!settings && plugin.requiredSecrets.length > 0 ? (
            <div className="flex flex-col gap-2">
              <h3 className="text-sm font-medium">Needs configuring</h3>
              <ul className="flex max-w-md flex-col gap-1">
                {plugin.requiredSecrets.map((secret) => (
                  <li
                    key={secret.key}
                    className="flex flex-col gap-0.5 text-sm sm:flex-row sm:items-baseline sm:justify-between sm:gap-3"
                  >
                    <span>{secret.name}</span>
                    <span className="font-mono text-xs text-muted-foreground">
                      {secret.key}
                    </span>
                  </li>
                ))}
              </ul>
            </div>
          ) : null}

          {plugin.sections.description ? (
            <PublishedMarkdown>{plugin.sections.description}</PublishedMarkdown>
          ) : null}

          {/* Installation steps answer a question an installed plugin has
              already settled. */}
          {!plugin.installed && plugin.sections.installation ? (
            <div className="flex flex-col gap-2">
              <h3 className="text-sm font-medium">Installing</h3>
              <PublishedMarkdown>
                {plugin.sections.installation}
              </PublishedMarkdown>
            </div>
          ) : null}

          {plugin.dependencies.length > 0 ? (
            <div className="flex flex-col gap-2">
              <h3 className="text-sm font-medium">Depends on</h3>
              <ul className="flex max-w-md flex-col gap-1">
                {plugin.dependencies.map((dep) => (
                  <li
                    key={dep.id}
                    className="flex items-baseline justify-between gap-3 text-sm"
                  >
                    <span className="font-mono text-xs">{dep.id}</span>
                    <span className="font-mono text-xs tabular-nums text-muted-foreground">
                      {dep.version}
                    </span>
                  </li>
                ))}
              </ul>
            </div>
          ) : null}
        </TabsContent>

        <TabsContent value="versions" className="flex flex-col gap-4 pt-4">
          {plugin.sections.changelog ? (
            <PublishedMarkdown>{plugin.sections.changelog}</PublishedMarkdown>
          ) : null}
          {plugin.releases.length === 0 ? (
            <p className="text-sm text-muted-foreground">
              The catalogue lists no releases for this plugin.
            </p>
          ) : (
            <ul className="flex max-w-xl flex-col divide-y rounded-lg border">
              {plugin.releases.map((release) => {
                const current = release.version === plugin.version;
                return (
                  <li key={release.version} className="flex flex-col gap-1 p-3">
                    <div className="flex flex-wrap items-baseline justify-between gap-3">
                      <span
                        className={cn(
                          "font-mono text-sm tabular-nums",
                          current && "font-medium",
                        )}
                      >
                        {release.version}
                      </span>
                      <div className="flex items-center gap-3">
                        {release.published_at ? (
                          <span className="text-xs text-muted-foreground">
                            {new Date(
                              release.published_at,
                            ).toLocaleDateString()}
                          </span>
                        ) : null}
                        <ReleaseStateBadge
                          state={release.state}
                          installable={release.installable}
                        />
                        {current ? (
                          <Badge variant="secondary">
                            {plugin.installed
                              ? "Installed"
                              : `On ${plugin.registry?.channel ?? "latest"}`}
                          </Badge>
                        ) : null}
                      </div>
                    </div>
                    {release.reason && release.state !== "active" ? (
                      <p className="text-xs text-muted-foreground">
                        {release.reason}
                      </p>
                    ) : null}
                  </li>
                );
              })}
            </ul>
          )}
        </TabsContent>

        <TabsContent value="permissions" className="flex flex-col gap-4 pt-4">
          <p className="max-w-prose text-sm text-muted-foreground">
            {plugin.installed
              ? "What this plugin declared. Nothing else is granted."
              : "Installing grants exactly this and nothing more."}
          </p>
          <PluginCapabilities
            entries={capabilities}
            allowedHosts={plugin.allowedHosts}
          />
          {plugin.sections.security ? (
            <div className="flex flex-col gap-2 pt-2">
              <h3 className="text-sm font-medium">From the publisher</h3>
              <PublishedMarkdown>{plugin.sections.security}</PublishedMarkdown>
            </div>
          ) : null}

          {plugin.sha256 ? (
            <div className="flex flex-col gap-1 pt-2">
              <h3 className="text-xs text-muted-foreground">Checksum</h3>
              <code className="break-all font-mono text-xs text-muted-foreground">
                {plugin.sha256}
              </code>
            </div>
          ) : null}
          {plugin.source?.url ? (
            <div className="flex flex-col gap-1">
              <h3 className="text-xs text-muted-foreground">URL</h3>
              <a
                href={plugin.source.url}
                target="_blank"
                rel="noreferrer"
                className="font-mono text-xs underline decoration-muted-foreground/40 underline-offset-4 hover:decoration-current"
              >
                <span className="break-all">{plugin.source.url}</span>
                <ExternalLink className="ml-1 inline size-3 align-[-1px]" />
              </a>
            </div>
          ) : null}
        </TabsContent>

        {settings ? (
          <TabsContent value="settings" className="pt-4">
            {settings}
          </TabsContent>
        ) : null}
      </Tabs>
    </div>
  );
}

/** The one control whose state differs between the two routes. */
export function InstallAction({
  installed,
  installing,
  onInstall,
}: {
  installed: boolean;
  installing?: boolean;
  onInstall?: () => void;
}) {
  if (installed) {
    return (
      <Button disabled variant="secondary">
        <Check />
        Installed
      </Button>
    );
  }
  return (
    <Button onClick={onInstall} disabled={installing}>
      {installing ? <Loader2 className="animate-spin" /> : null}
      Install
    </Button>
  );
}
