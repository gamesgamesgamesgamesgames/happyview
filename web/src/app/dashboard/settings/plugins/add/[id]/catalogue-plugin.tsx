"use client";

import { useCallback, useEffect, useMemo, useState } from "react";
import Link from "next/link";
import { usePathname, useRouter, useSearchParams } from "next/navigation";

import { useOfficialPlugins } from "@/hooks/use-official-plugins";
import {
  addPlugin,
  getPlugins,
  previewPlugin,
  type PluginPreview,
} from "@/lib/api";
import { URL_PLUGIN_ID, installedPluginHref } from "@/lib/plugin-routes";
import { visibleCapabilities } from "@/components/plugin-capabilities";
import {
  InstallAction,
  PluginDetail,
  type PluginView,
} from "@/components/plugin-detail";
import { PageBreadcrumbs } from "@/components/page-breadcrumbs";
import { SiteHeader } from "@/components/site-header";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";

export default function CataloguePlugin() {
  // The `[id]` segment carries the URL-encoded plugin id, which contains dots.
  const id = decodeURIComponent(
    usePathname().split("/").filter(Boolean).pop() ?? "",
  );
  const search = useSearchParams();
  const router = useRouter();
  const { byId, loading: catalogueLoading } = useOfficialPlugins();

  const [preview, setPreview] = useState<PluginPreview | null>(null);
  const [installing, setInstalling] = useState(false);
  const [installed, setInstalled] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const entry = id === URL_PLUGIN_ID ? undefined : byId.get(id);
  const manifestUrl = search.get("manifest") ?? entry?.manifest_url ?? null;

  useEffect(() => {
    if (!manifestUrl) return;
    const controller = new AbortController();
    void previewPlugin(manifestUrl, controller.signal)
      .then(setPreview)
      .catch((e: unknown) => {
        if (controller.signal.aborted) return;
        setError(e instanceof Error ? e.message : String(e));
      });
    return () => controller.abort();
  }, [manifestUrl]);

  // Derived rather than held: a `loading` flag the effect sets would render
  // once with the wrong value before correcting itself.
  const reading = manifestUrl !== null && preview === null && error === null;

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
    };
  }, [preview, entry, installed]);

  const install = useCallback(async () => {
    if (!preview) return;
    setInstalling(true);
    setError(null);
    try {
      await addPlugin({
        url: preview.manifest_url,
        sha256: preview.sha256,
        accepted_capabilities: visibleCapabilities(preview.capabilities).map(
          (c) => c.name,
        ),
      });
      router.push(installedPluginHref(preview.id));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setInstalling(false);
    }
  }, [preview, router]);

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

        {catalogueLoading || reading ? (
          <div className="flex flex-col gap-4">
            <Skeleton className="h-24 w-full rounded-lg" />
            <Skeleton className="h-40 w-full rounded-lg" />
          </div>
        ) : !view ? (
          <div className="flex flex-col items-start gap-3 rounded-lg border border-dashed p-8">
            <p className="text-sm font-medium">
              {manifestUrl
                ? "Couldn't read that manifest"
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
