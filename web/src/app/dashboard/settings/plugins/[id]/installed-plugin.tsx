"use client";

import { useCallback, useEffect, useMemo, useState } from "react";
import Link from "next/link";
import { usePathname, useRouter } from "next/navigation";
import { ArrowUpCircle, Loader2, RefreshCw, Trash2 } from "lucide-react";

import { useCurrentUser } from "@/hooks/use-current-user";
import { useOfficialPlugins } from "@/hooks/use-official-plugins";
import {
  checkPluginUpdate,
  getPluginAllowedHosts,
  getPluginSecrets,
  getPlugins,
  reloadPlugin,
  removePlugin,
  updatePluginAllowedHosts,
  updatePluginSecrets,
} from "@/lib/api";
import type { PluginSummary } from "@/types/plugins";
import { visibleCapabilities } from "@/components/plugin-capabilities";
import { PluginDetail, type PluginView } from "@/components/plugin-detail";
import { PluginUpdateDialog } from "@/components/plugin-update-dialog";
import { PageBreadcrumbs } from "@/components/page-breadcrumbs";
import { SiteHeader } from "@/components/site-header";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Skeleton } from "@/components/ui/skeleton";

/** Only `network:request:defined` has a host list the operator may set. */
function managesHosts(plugin: PluginSummary): boolean {
  return visibleCapabilities(plugin.capabilities).some(
    (c) => c.name === "network:request:defined",
  );
}

export default function InstalledPlugin() {
  // The `[id]` segment carries the URL-encoded plugin id, which contains dots.
  const id = decodeURIComponent(
    usePathname().split("/").filter(Boolean).pop() ?? "",
  );
  const router = useRouter();
  const { hasPermission } = useCurrentUser();
  const { byId } = useOfficialPlugins();

  const [plugin, setPlugin] = useState<PluginSummary | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<"reload" | "check" | "remove" | null>(null);

  const [secrets, setSecrets] = useState<Record<string, string>>({});
  const [hosts, setHosts] = useState<string[]>([]);
  const [saving, setSaving] = useState(false);
  const [updateOpen, setUpdateOpen] = useState(false);

  const canManage = hasPermission("plugins:manage");

  const load = useCallback(async () => {
    try {
      const response = await getPlugins();
      setPlugin(response.plugins.find((p) => p.id === id) ?? null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, [id]);

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    if (!plugin) return;
    let cancelled = false;
    void (async () => {
      try {
        const stored = await getPluginSecrets(plugin.id);
        const next: Record<string, string> = {};
        for (const secret of plugin.required_secrets) {
          next[secret.key] = stored.secrets[secret.key] ?? "";
        }
        if (cancelled) return;
        setSecrets(next);
        if (managesHosts(plugin)) {
          const answered = await getPluginAllowedHosts(plugin.id);
          if (cancelled) return;
          setHosts([...answered.hosts, ""]);
        } else {
          setHosts([]);
        }
      } catch {
        // Configuration is a tab, not the page. Failing to read it should not
        // take the plugin's identity down with it.
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [plugin]);

  const view: PluginView | null = useMemo(() => {
    if (!plugin) return null;
    const catalogue = byId.get(plugin.id);
    return {
      id: plugin.id,
      name: plugin.name,
      version: plugin.version,
      description: plugin.description,
      iconUrl: plugin.icon_url,
      bannerUrl: plugin.banner_url,
      type: plugin.plugin_type,
      namespace: plugin.namespace,
      capabilities: plugin.capabilities,
      dependencies: plugin.dependencies,
      allowedHosts: plugin.allowed_hosts,
      requiredSecrets: plugin.required_secrets,
      sha256: plugin.sha256,
      source: { kind: plugin.source, url: plugin.url },
      installed: true,
      updateAvailable: plugin.update_available,
      latestVersion: plugin.latest_version,
      releases: catalogue?.releases ?? plugin.pending_releases,
      sections: catalogue?.sections ?? {},
    };
  }, [plugin, byId]);

  async function run(
    kind: "reload" | "check" | "remove",
    action: () => Promise<unknown>,
  ) {
    setBusy(kind);
    setError(null);
    try {
      await action();
      if (kind === "remove") {
        router.push("/dashboard/settings/plugins");
        return;
      }
      await load();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  }

  async function saveSettings() {
    if (!plugin) return;
    setSaving(true);
    setError(null);
    try {
      await updatePluginSecrets(plugin.id, secrets);
      if (managesHosts(plugin)) {
        const trimmed = hosts.map((h) => h.trim()).filter(Boolean);
        const answered = await updatePluginAllowedHosts(plugin.id, trimmed);
        setHosts([...answered.hosts, ""]);
      }
      await load();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  }

  const hasSettings =
    plugin !== null &&
    canManage &&
    (plugin.required_secrets.length > 0 || managesHosts(plugin));

  return (
    <>
      <SiteHeader title="Plugins" />
      <PageBreadcrumbs
        crumbs={[
          { label: "Plugins", href: "/dashboard/settings/plugins" },
          { label: plugin?.name ?? id },
        ]}
      />
      <div className="flex flex-1 flex-col gap-4 p-4 md:p-6">
        {error ? (
          <p role="alert" className="text-sm text-destructive">
            {error}
          </p>
        ) : null}

        {loading ? (
          <div className="flex flex-col gap-4">
            <Skeleton className="h-24 w-full rounded-lg" />
            <Skeleton className="h-40 w-full rounded-lg" />
          </div>
        ) : !view ? (
          <div className="flex flex-col items-start gap-3 rounded-lg border border-dashed p-8">
            <p className="text-sm font-medium">No plugin with that id</p>
            <p className="text-sm text-muted-foreground">
              <span className="font-mono text-xs">{id}</span> is not installed.
            </p>
            <Button variant="outline" size="sm" asChild>
              <Link href="/dashboard/settings/plugins">Installed plugins</Link>
            </Button>
          </div>
        ) : (
          <PluginDetail
            plugin={view}
            actions={
              canManage ? (
                <>
                  {plugin?.update_available ? (
                    <Button onClick={() => setUpdateOpen(true)}>
                      <ArrowUpCircle />
                      Update
                    </Button>
                  ) : null}
                  <Button
                    variant="outline"
                    onClick={() => void run("reload", () => reloadPlugin(id))}
                    disabled={busy !== null}
                  >
                    {busy === "reload" ? (
                      <Loader2 className="animate-spin" />
                    ) : (
                      <RefreshCw />
                    )}
                    Reload
                  </Button>
                  <Button
                    variant="outline"
                    onClick={() =>
                      void run("check", () => checkPluginUpdate(id))
                    }
                    disabled={busy !== null}
                  >
                    {busy === "check" ? (
                      <Loader2 className="animate-spin" />
                    ) : null}
                    Check for update
                  </Button>
                  <Button
                    variant="destructive"
                    onClick={() => void run("remove", () => removePlugin(id))}
                    disabled={busy !== null}
                  >
                    {busy === "remove" ? (
                      <Loader2 className="animate-spin" />
                    ) : (
                      <Trash2 />
                    )}
                    Remove
                  </Button>
                </>
              ) : null
            }
            settings={
              hasSettings && plugin ? (
                <div className="flex max-w-xl flex-col gap-4">
                  {plugin.required_secrets.map((secret) => (
                    <div key={secret.key} className="flex flex-col gap-2">
                      <Label htmlFor={`secret-${secret.key}`}>
                        {secret.name}
                      </Label>
                      <Input
                        id={`secret-${secret.key}`}
                        type="password"
                        autoComplete="off"
                        placeholder={secret.key}
                        value={secrets[secret.key] ?? ""}
                        onChange={(e) =>
                          setSecrets((prev) => ({
                            ...prev,
                            [secret.key]: e.target.value,
                          }))
                        }
                      />
                      {secret.description ? (
                        <p className="text-xs text-muted-foreground">
                          {secret.description}
                        </p>
                      ) : null}
                    </div>
                  ))}

                  {managesHosts(plugin) ? (
                    <div className="flex flex-col gap-2">
                      <Label>Allowed hosts</Label>
                      {hosts.map((host, index) => (
                        <Input
                          key={index}
                          value={host}
                          placeholder="api.example.com"
                          onChange={(e) => {
                            const next = [...hosts];
                            next[index] = e.target.value;
                            setHosts([...next.filter((h) => h.trim()), ""]);
                          }}
                        />
                      ))}
                      <p className="text-xs text-muted-foreground">
                        A leading <code className="font-mono">*.</code> allows
                        subdomains.
                      </p>
                    </div>
                  ) : null}

                  <div className="flex items-center gap-3">
                    <Button onClick={saveSettings} disabled={saving} size="sm">
                      {saving ? <Loader2 className="animate-spin" /> : null}
                      Save
                    </Button>
                    {plugin.secrets_configured ? null : (
                      <span className="text-xs text-muted-foreground">
                        Not configured
                      </span>
                    )}
                  </div>
                </div>
              ) : undefined
            }
          />
        )}
      </div>

      <PluginUpdateDialog
        plugin={plugin}
        open={updateOpen}
        onOpenChange={setUpdateOpen}
        onUpdated={() => {
          setUpdateOpen(false);
          void load();
        }}
      />
    </>
  );
}
