"use client";

import { useCallback, useEffect, useMemo, useState } from "react";
import Link from "next/link";
import { usePathname, useRouter, useSearchParams } from "next/navigation";

import { useOfficialPlugins } from "@/hooks/use-official-plugins";
import {
  addPlugin,
  getPlugins,
  previewPlugin,
  type PluginOrigin,
  type PluginPreview,
} from "@/lib/api";
import { URL_PLUGIN_ID, installedPluginHref } from "@/lib/plugin-routes";
import { visibleCapabilities } from "@/components/plugin-capabilities";
import {
  InstallAction,
  PluginDetail,
  type PluginView,
} from "@/components/plugin-detail";
import {
  ChannelSelect,
  LifecycleNotice,
  ReleaseStateBadge,
  orderedChannels,
  unavailableChannelCopy,
} from "@/components/plugin-lifecycle";
import { PageBreadcrumbs } from "@/components/page-breadcrumbs";
import { SiteHeader } from "@/components/site-header";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import { Skeleton } from "@/components/ui/skeleton";

const DEFAULT_CHANNEL = "latest";

/** A preview and the origin it was read from, so an answer for a channel no
 * longer selected is never shown as the current one. */
type Reading = {
  key: string;
  preview: PluginPreview | null;
  error: string | null;
};

export default function CataloguePlugin() {
  // The `[id]` segment carries the URL-encoded plugin id, which contains dots.
  const id = decodeURIComponent(
    usePathname().split("/").filter(Boolean).pop() ?? "",
  );
  const search = useSearchParams();
  const router = useRouter();
  const { byId, loading: catalogueLoading } = useOfficialPlugins();

  const [chosenChannel, setChosenChannel] = useState<string | null>(null);
  const [reading, setReading] = useState<Reading | null>(null);
  const [installing, setInstalling] = useState(false);
  const [installed, setInstalled] = useState(false);
  const [installError, setInstallError] = useState<string | null>(null);

  const entry = id === URL_PLUGIN_ID ? undefined : byId.get(id);
  const pastedUrl = search.get("manifest");
  const retired = entry?.lifecycle_state === "retired";
  // A catalogue entry is installed by its registry project. Only a pasted
  // URL, or an entry with no publisher to name, is installed from a URL.
  const byProject = Boolean(entry?.publisher_did) && !pastedUrl;

  const channelNames = useMemo(
    () => (entry ? orderedChannels(Object.keys(entry.channels)) : []),
    [entry],
  );
  // `latest` unless the project doesn't publish one; a choice wins.
  const channel =
    chosenChannel ??
    (channelNames.includes(DEFAULT_CHANNEL)
      ? DEFAULT_CHANNEL
      : (channelNames[0] ?? DEFAULT_CHANNEL));
  const head = entry?.channels[channel];
  const headRelease = entry?.releases.find((r) => r.version === head?.version);

  // Serialised so the preview effect runs when the origin changes, not each
  // time the catalogue poll hands back an equal entry.
  const originKey = useMemo(() => {
    let origin: PluginOrigin | null = null;
    if (pastedUrl) {
      origin = { url: pastedUrl };
    } else if (entry && !retired) {
      if (entry.publisher_did) {
        // The server refuses to preview a version that may not be installed.
        if (head?.installable) {
          origin = {
            registry: { did: entry.publisher_did, name: entry.id, channel },
          };
        }
      } else {
        origin = { url: entry.manifest_url };
      }
    }
    return origin ? JSON.stringify(origin) : null;
  }, [pastedUrl, entry, retired, head, channel]);

  useEffect(() => {
    if (!originKey) return;
    const origin = JSON.parse(originKey) as PluginOrigin;
    const controller = new AbortController();
    void previewPlugin(origin, controller.signal)
      .then((preview) => setReading({ key: originKey, preview, error: null }))
      .catch((e: unknown) => {
        if (controller.signal.aborted) return;
        setReading({
          key: originKey,
          preview: null,
          error: e instanceof Error ? e.message : String(e),
        });
      });
    return () => controller.abort();
  }, [originKey]);

  const current = reading && reading.key === originKey ? reading : null;
  const preview = current?.preview ?? null;
  // Derived rather than held: a `loading` flag the effect sets would render
  // once with the wrong value before correcting itself.
  const loadingPreview = originKey !== null && current === null;
  const error = installError ?? current?.error ?? null;

  useEffect(() => {
    if (!preview) return;
    void getPlugins()
      .then((r) => setInstalled(r.plugins.some((p) => p.id === preview.id)))
      // Not knowing costs an Install button that answers "already installed"
      // on click, which the server refuses safely.
      .catch(() => setInstalled(false));
  }, [preview]);

  const view: PluginView | null = useMemo(() => {
    if (!preview) return null;
    const registry = preview.registry;
    return {
      id: preview.id,
      name: preview.name,
      version: preview.version,
      description: preview.description,
      iconUrl: preview.icon_url ?? entry?.icon_url ?? null,
      // The preview reads the manifest, which has no banner field; the
      // catalogue is the only place one is published.
      bannerUrl: entry?.banner_url ?? null,
      type: preview.plugin_type,
      namespace: preview.namespace,
      capabilities: preview.capabilities,
      dependencies: preview.dependencies,
      allowedHosts: preview.allowed_hosts,
      requiredSecrets: preview.required_secrets,
      sha256: preview.sha256,
      source: { kind: "url", url: preview.manifest_url },
      installed,
      updateAvailable: false,
      latestVersion: null,
      releases: entry?.releases ?? [],
      sections: entry?.sections ?? {},
      registry: registry
        ? {
            did: registry.did,
            channel: registry.channel,
            state: registry.state,
            reason:
              entry?.releases.find(
                (r) => r.version_uri === registry.version_uri,
              )?.reason ?? null,
            unresolved: false,
          }
        : null,
    };
  }, [preview, entry, installed]);

  const install = useCallback(async () => {
    if (!preview || !originKey) return;
    const origin = JSON.parse(originKey) as PluginOrigin;
    setInstalling(true);
    setInstallError(null);
    const accepted_capabilities = visibleCapabilities(preview.capabilities).map(
      (c) => c.name,
    );
    try {
      await addPlugin(
        "registry" in origin
          ? { registry: origin.registry, accepted_capabilities }
          : {
              url: preview.manifest_url,
              sha256: preview.sha256,
              accepted_capabilities,
            },
      );
      router.push(installedPluginHref(preview.id));
    } catch (e) {
      setInstallError(e instanceof Error ? e.message : String(e));
      setInstalling(false);
    }
  }, [preview, originKey, router]);

  const channelPicker =
    byProject && !retired && channelNames.length > 0 ? (
      <div className="flex flex-wrap items-center gap-3">
        <Label htmlFor="plugin-channel">Channel</Label>
        <ChannelSelect
          id="plugin-channel"
          channels={entry?.channels ?? null}
          value={channel}
          onValueChange={(next) => {
            setChosenChannel(next);
            setInstallError(null);
          }}
          disabled={installing}
        />
        {head ? (
          <ReleaseStateBadge
            state={head.state}
            installable={head.installable}
          />
        ) : null}
      </div>
    ) : null;

  // What stops an install, if anything does. Each keeps the channel picker
  // beside it, so another channel is one choice away.
  const blocked = retired ? (
    <LifecycleNotice
      tone="danger"
      title="This plugin is retired by its publisher"
    >
      It can no longer be installed from the registry.
    </LifecycleNotice>
  ) : byProject && channelNames.length === 0 ? (
    <LifecycleNotice
      tone="warning"
      title="The registry lists no channels for this plugin"
    >
      There is no version to install yet.
    </LifecycleNotice>
  ) : byProject && head && !head.installable ? (
    <LifecycleNotice
      tone={head.state === "retired" ? "danger" : "warning"}
      title={unavailableChannelCopy(channel, head.state)}
    >
      {headRelease?.reason ??
        (channelNames.length > 1
          ? "Pick another channel to install from."
          : "There is no version to install from it.")}
    </LifecycleNotice>
  ) : null;

  // Installable, but the publisher has said something an operator should
  // read before choosing it.
  const caution =
    entry?.lifecycle_state === "deprecated" ? (
      <LifecycleNotice
        tone="warning"
        title="This plugin is deprecated by its publisher"
      />
    ) : byProject && head?.installable && head.state === "deprecated" ? (
      <LifecycleNotice
        tone="warning"
        title={
          headRelease?.reason
            ? `Deprecated: ${headRelease.reason}`
            : "This version is deprecated by its publisher"
        }
      >
        It still installs, but its publisher recommends against it.
      </LifecycleNotice>
    ) : null;

  return (
    <>
      <SiteHeader title="Add plugin" />
      <PageBreadcrumbs
        crumbs={[
          { label: "Plugins", href: "/dashboard/settings/plugins" },
          { label: "Add plugin", href: "/dashboard/settings/plugins/add" },
          { label: preview?.name ?? entry?.name ?? "Plugin" },
        ]}
      />
      <div className="flex flex-1 flex-col gap-4 p-4 md:p-6">
        {error ? (
          <p
            role="alert"
            className="rounded-md border border-destructive/50 bg-destructive/10 p-3 text-sm text-destructive"
          >
            {error}
          </p>
        ) : null}

        {catalogueLoading ? null : channelPicker}
        {catalogueLoading || blocked ? null : caution}

        {catalogueLoading || loadingPreview ? (
          <div className="flex flex-col gap-4">
            <Skeleton className="h-24 w-full rounded-lg" />
            <Skeleton className="h-40 w-full rounded-lg" />
          </div>
        ) : blocked ? (
          <div className="flex flex-col items-start gap-4">
            {blocked}
            <Button variant="outline" size="sm" asChild>
              <Link href="/dashboard/settings/plugins/add">Find a plugin</Link>
            </Button>
          </div>
        ) : !view ? (
          <div className="flex flex-col items-start gap-3 rounded-lg border border-dashed p-8">
            <p className="text-sm font-medium">
              {pastedUrl
                ? "Couldn't read that manifest"
                : entry
                  ? "Couldn't read this plugin from the registry"
                  : "The registry doesn't list that plugin"}
            </p>
            <Button variant="outline" size="sm" asChild>
              <Link href="/dashboard/settings/plugins/add">Find a plugin</Link>
            </Button>
          </div>
        ) : (
          <PluginDetail
            plugin={view}
            actions={
              <InstallAction
                installed={installed}
                installing={installing}
                onInstall={() => void install()}
              />
            }
          />
        )}
      </div>
    </>
  );
}
