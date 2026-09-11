"use client";

import { useCallback, useEffect, useMemo, useState } from "react";
import { useSearchParams } from "next/navigation";
import { Plus, Trash2, RefreshCw, ExternalLink, Settings, Loader2, AlertTriangle, CheckCircle2, AlertCircle, ArrowUpCircle, Search, ChevronDown } from "lucide-react";

import { useCurrentUser } from "@/hooks/use-current-user";
import { useOfficialPlugins } from "@/hooks/use-official-plugins";
import { getPlugins, addPlugin, removePlugin, reloadPlugin, getPluginSecrets, updatePluginSecrets, previewPlugin, checkPluginUpdate, type PluginPreview } from "@/lib/api";
import type { PluginSummary } from "@/types/plugins";
import { PluginUpdateDialog } from "@/components/plugin-update-dialog";
import { PluginCapabilities, visibleCapabilities } from "@/components/plugin-capabilities";
import { SiteHeader } from "@/components/site-header";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Badge } from "@/components/ui/badge";
import {
  Command,
  CommandGroup,
  CommandItem,
  CommandList,
} from "@/components/ui/command";
import {
  Popover,
  PopoverAnchor,
  PopoverContent,
} from "@/components/ui/popover";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@/components/ui/collapsible";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import {
  ResponsiveDialog,
  ResponsiveDialogClose,
  ResponsiveDialogContent,
  ResponsiveDialogDescription,
  ResponsiveDialogFooter,
  ResponsiveDialogHeader,
  ResponsiveDialogTitle,
  ResponsiveDialogTrigger,
} from "@/components/ui/responsive-dialog";

function isValidHttpUrl(value: string): boolean {
  try {
    const parsed = new URL(value);
    return parsed.protocol === "http:" || parsed.protocol === "https:";
  } catch {
    return false;
  }
}

function formatAuthType(authType: string): string {
  const formats: Record<string, string> = {
    oauth2: "OAuth 2.0",
    openid: "OpenID",
    api_key: "API Key",
  };
  return formats[authType] || authType;
}

export default function PluginsPage() {
  const { hasPermission } = useCurrentUser();
  const [plugins, setPlugins] = useState<PluginSummary[]>([]);
  const [encryptionConfigured, setEncryptionConfigured] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [reloading, setReloading] = useState<string | null>(null);
  const [removing, setRemoving] = useState<string | null>(null);
  const [updateDialogPlugin, setUpdateDialogPlugin] = useState<PluginSummary | null>(null);
  const [updateDialogOpen, setUpdateDialogOpen] = useState(false);
  const [checkingUpdate, setCheckingUpdate] = useState<string | null>(null);

  const searchParams = useSearchParams();

  // Add plugin dialog state
  const [addOpen, setAddOpen] = useState(false);
  const [newUrl, setNewUrl] = useState("");
  const [adding, setAdding] = useState(false);
  const [pluginPreview, setPluginPreview] = useState<PluginPreview | null>(null);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [comboboxOpen, setComboboxOpen] = useState<boolean>(false);
  const [selectedManifestUrl, setSelectedManifestUrl] = useState<string | null>(
    null,
  );

  const { plugins: officialPlugins, loading: officialLoading } = useOfficialPlugins();

  const filteredOfficialPlugins = useMemo(() => {
    const q = newUrl.trim().toLowerCase();
    if (!q) return officialPlugins;
    return officialPlugins.filter(
      (p) =>
        p.id.toLowerCase().includes(q) ||
        p.name.toLowerCase().includes(q),
    );
  }, [officialPlugins, newUrl]);

  const showCombobox = officialLoading || officialPlugins.length > 0;
  const hasComboboxResults =
    officialLoading || filteredOfficialPlugins.length > 0;

  // Configure secrets dialog state
  const [configOpen, setConfigOpen] = useState(false);
  const [configPlugin, setConfigPlugin] = useState<PluginSummary | null>(null);
  const [secretValues, setSecretValues] = useState<Record<string, string>>({});
  const [savingSecrets, setSavingSecrets] = useState(false);

  const canCreate = hasPermission("plugins:create");
  const canDelete = hasPermission("plugins:delete");

  const load = useCallback(async () => {
    try {
      const response = await getPlugins();
      setPlugins(response.plugins ?? []);
      setEncryptionConfigured(response.encryption_configured ?? false);
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  function openUpdateDialog(plugin: PluginSummary) {
    setUpdateDialogPlugin(plugin);
    setUpdateDialogOpen(true);
  }

  async function handleCheckUpdate(plugin: PluginSummary) {
    setCheckingUpdate(plugin.id);
    setError(null);
    try {
      await checkPluginUpdate(plugin.id);
      await load();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setCheckingUpdate(null);
    }
  }

  useEffect(() => {
    const updateId = searchParams.get("update");
    if (!updateId || plugins.length === 0) return;
    const target = plugins.find((p) => p.id === updateId);
    if (target && target.update_available) {
      openUpdateDialog(target);
    }
  }, [searchParams, plugins]);

  const newUrlIsUrl = isValidHttpUrl(newUrl.trim());
  const effectivePreviewUrl = selectedManifestUrl
    ? selectedManifestUrl
    : newUrlIsUrl
      ? newUrl.trim()
      : null;

  useEffect(() => {
    if (!addOpen) return;
    if (!effectivePreviewUrl) {
      setPluginPreview(null);
      setPreviewError(null);
      return;
    }
    const controller = new AbortController();
    setPreviewError(null);
    const timer = setTimeout(async () => {
      try {
        const preview = await previewPlugin(
          effectivePreviewUrl,
          controller.signal,
        );
        if (!controller.signal.aborted) {
          setPluginPreview(preview);
          setPreviewError(null);
        }
      } catch (e) {
        if (!controller.signal.aborted) {
          setPluginPreview(null);
          setPreviewError(e instanceof Error ? e.message : String(e));
        }
      }
    }, 500);
    return () => {
      clearTimeout(timer);
      controller.abort();
    };
  }, [addOpen, effectivePreviewUrl]);

  async function handleAdd() {
    if (!pluginPreview) return;

    setAdding(true);
    setError(null);
    try {
      const capabilities = visibleCapabilities(pluginPreview.capabilities);
      await addPlugin({
        url: pluginPreview.wasm_url,
        sha256: pluginPreview.sha256,
        accepted_capabilities: capabilities.map((c) => c.name),
      });
      setAddOpen(false);
      setNewUrl("");
      setPluginPreview(null);
      setPreviewError(null);
      setSelectedManifestUrl(null);
      load();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setAdding(false);
    }
  }

  function handleCancelAdd() {
    setAddOpen(false);
    setNewUrl("");
    setPluginPreview(null);
    setPreviewError(null);
    setSelectedManifestUrl(null);
    setComboboxOpen(false);
    setError(null);
  }

  async function handleReload(id: string) {
    setReloading(id);
    setError(null);
    try {
      await reloadPlugin(id);
      load();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setReloading(null);
    }
  }

  async function handleRemove(id: string) {
    setRemoving(id);
    setError(null);
    try {
      await removePlugin(id);
      load();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setRemoving(null);
    }
  }

  async function handleOpenConfig(plugin: PluginSummary) {
    setConfigPlugin(plugin);
    setError(null);
    try {
      const response = await getPluginSecrets(plugin.id);
      // Initialize with existing secrets (masked) and empty strings for missing ones
      const initial: Record<string, string> = {};
      for (const secret of plugin.required_secrets) {
        initial[secret.key] = response.secrets[secret.key] || "";
      }
      setSecretValues(initial);
      setConfigOpen(true);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }

  async function handleSaveSecrets() {
    if (!configPlugin) return;
    setSavingSecrets(true);
    setError(null);
    try {
      await updatePluginSecrets(configPlugin.id, secretValues);
      setConfigOpen(false);
      setConfigPlugin(null);
      setSecretValues({});
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSavingSecrets(false);
    }
  }

  return (
    <>
      <SiteHeader title="Plugins" />
      <div className="flex flex-1 flex-col gap-6 p-4 md:p-6">
        {error && <p className="text-destructive text-sm">{error}</p>}

        {!encryptionConfigured && (
          <div className="flex items-start gap-3 rounded-lg border border-amber-500/50 bg-amber-500/10 p-4">
            <AlertTriangle className="size-5 text-amber-500 shrink-0 mt-0.5" />
            <div>
              <p className="font-medium text-amber-500">Encryption not configured</p>
              <p className="text-sm text-muted-foreground mt-1">
                Plugin secrets cannot be stored without an encryption key. Set the{" "}
                <code className="text-xs bg-muted px-1 py-0.5 rounded">TOKEN_ENCRYPTION_KEY</code>{" "}
                environment variable to a base64-encoded 32-byte key.
              </p>
              <p className="text-sm text-muted-foreground mt-1">
                Generate one with: <code className="text-xs bg-muted px-1 py-0.5 rounded">openssl rand -base64 32</code>
              </p>
            </div>
          </div>
        )}

        <div className="flex items-center justify-between">
          <div>
            <h2 className="text-lg font-semibold">External Auth Plugins</h2>
            <p className="text-muted-foreground text-sm">
              Manage WASM plugins that provide authentication with external platforms.
            </p>
          </div>
          {canCreate && (
            <ResponsiveDialog open={addOpen} onOpenChange={(open) => {
              if (!open) handleCancelAdd();
              else setAddOpen(true);
            }}>
              <ResponsiveDialogTrigger asChild>
                <Button size="sm">
                  <Plus className="mr-1 size-4" />
                  Add Plugin
                </Button>
              </ResponsiveDialogTrigger>
              <ResponsiveDialogContent>
                <ResponsiveDialogHeader>
                  <ResponsiveDialogTitle>Add Plugin</ResponsiveDialogTitle>
                  <ResponsiveDialogDescription>
                    Select an official plugin or enter a plugin URL.
                  </ResponsiveDialogDescription>
                </ResponsiveDialogHeader>

                <div className="grid gap-4 py-4">
                  <div className="grid gap-2">
                    <Label htmlFor="url">Plugin</Label>
                      {showCombobox ? (
                        <Popover
                          open={
                            comboboxOpen && hasComboboxResults && !newUrlIsUrl
                          }
                          onOpenChange={setComboboxOpen}
                        >
                          <PopoverAnchor asChild>
                            <Input
                              id="url"
                              placeholder="https://github.com/org/repo/releases/download/v1.0.0/steam.wasm"
                              value={newUrl}
                              onChange={(e) => {
                                setNewUrl(e.target.value);
                                setSelectedManifestUrl(null);
                                setComboboxOpen(true);
                              }}
                              onFocus={() => setComboboxOpen(true)}
                              autoComplete="off"
                            />
                          </PopoverAnchor>
                          <PopoverContent
                            className="p-0 w-(--radix-popover-trigger-width)"
                            align="start"
                            onOpenAutoFocus={(e) => e.preventDefault()}
                            onInteractOutside={(e) => {
                              // Don't close when clicking the input itself
                              const target = e.target as Node;
                              if (
                                target instanceof Element &&
                                target.id === "url"
                              ) {
                                e.preventDefault();
                              }
                            }}
                          >
                            <Command shouldFilter={false}>
                              <CommandList>
                                {officialLoading ? (
                                  <CommandGroup>
                                    <CommandItem
                                      disabled
                                      value="__loading__"
                                    >
                                      <Loader2 className="mr-2 size-4 animate-spin" />
                                      Loading plugins…
                                    </CommandItem>
                                  </CommandGroup>
                                ) : (
                                  <CommandGroup>
                                      {filteredOfficialPlugins.map((p) => (
                                        <CommandItem
                                          key={p.id}
                                          value={`${p.id} ${p.name}`}
                                          onSelect={() => {
                                            setNewUrl(p.name);
                                            setSelectedManifestUrl(
                                              p.manifest_url,
                                            );
                                            setComboboxOpen(false);
                                          }}
                                        >
                                          {p.icon_url ? (
                                            // eslint-disable-next-line @next/next/no-img-element
                                            <img
                                              src={p.icon_url}
                                              alt=""
                                              className="size-6 rounded shrink-0"
                                            />
                                          ) : (
                                            <div className="size-6 rounded bg-muted shrink-0" />
                                          )}
                                          <div className="flex flex-col min-w-0 flex-1">
                                            <div className="flex items-center gap-2">
                                              <span className="font-medium truncate">
                                                {p.name}
                                              </span>
                                              <Badge
                                                variant="secondary"
                                                className="shrink-0"
                                              >
                                                v{p.latest_version}
                                              </Badge>
                                            </div>
                                            {p.description && (
                                              <span className="text-muted-foreground text-xs truncate">
                                                {p.description}
                                              </span>
                                            )}
                                          </div>
                                        </CommandItem>
                                      ))}
                                    </CommandGroup>
                                )}
                              </CommandList>
                            </Command>
                          </PopoverContent>
                        </Popover>
                      ) : (
                        <Input
                          id="url"
                          placeholder="https://github.com/org/repo/releases/download/v1.0.0/steam.wasm"
                          value={newUrl}
                          onChange={(e) => {
                            setNewUrl(e.target.value);
                            setSelectedManifestUrl(null);
                          }}
                        />
                      )}
                    <p className="text-muted-foreground text-xs">
                      Link to the .wasm file or manifest.json (GitHub Releases URL)
                    </p>
                  </div>

                  {previewError && (
                    <p className="text-sm text-destructive">{previewError}</p>
                  )}

                  {pluginPreview && (
                    <div className="grid gap-4 rounded-lg border p-4">
                      <div className="flex items-start gap-4">
                        {pluginPreview.icon_url && (
                          <img
                            src={pluginPreview.icon_url}
                            alt=""
                            className="size-12 rounded"
                          />
                        )}
                        <div className="flex-1">
                          <h3 className="font-semibold">{pluginPreview.name}</h3>
                          <p className="text-muted-foreground text-sm">
                            {pluginPreview.description || `Version ${pluginPreview.version}`}
                          </p>
                        </div>
                        <Badge variant="secondary">{pluginPreview.version}</Badge>
                      </div>

                      <div className="grid gap-2 text-sm">
                        <div className="flex justify-between">
                          <span className="text-muted-foreground">Auth Type</span>
                          <Badge variant="outline">
                            {formatAuthType(pluginPreview.auth_type)}
                          </Badge>
                        </div>
                        {pluginPreview.required_secrets.length > 0 && (
                          <div className="grid gap-2">
                            <span className="text-muted-foreground">Required Configuration</span>
                            <div className="flex flex-col gap-2">
                              {pluginPreview.required_secrets.map((secret) => (
                                <div key={secret.key} className="bg-muted rounded p-2">
                                  <div className="flex items-center justify-between">
                                    <span className="font-medium text-sm">{secret.name}</span>
                                    <code className="text-xs text-muted-foreground">{secret.key}</code>
                                  </div>
                                  {secret.description && (
                                    <p className="text-muted-foreground text-xs mt-1">{secret.description}</p>
                                  )}
                                </div>
                              ))}
                            </div>
                          </div>
                        )}
                      </div>

                      <div className="space-y-2">
                        <h4 className="text-sm font-medium">This plugin can</h4>
                        <PluginCapabilities
                          entries={visibleCapabilities(pluginPreview.capabilities)}
                          allowedHosts={pluginPreview.allowed_hosts}
                        />
                        {visibleCapabilities(pluginPreview.capabilities).some(
                          (c) => c.risk === "critical" || c.risk === "high",
                        ) && (
                          <p className="text-sm text-destructive">
                            This plugin asks for dangerous permissions. Only install it if you trust its publisher.
                          </p>
                        )}
                      </div>
                    </div>
                  )}
                </div>

                <ResponsiveDialogFooter>
                  <ResponsiveDialogClose asChild>
                    <Button variant="outline" disabled={adding}>
                      Cancel
                    </Button>
                  </ResponsiveDialogClose>
                  <Button onClick={handleAdd} disabled={adding || !pluginPreview}>
                    {adding ? (
                      <>
                        <Loader2 className="mr-2 size-4 animate-spin" />
                        Installing...
                      </>
                    ) : (
                      "Install Plugin"
                    )}
                  </Button>
                </ResponsiveDialogFooter>
              </ResponsiveDialogContent>
            </ResponsiveDialog>
          )}
        </div>

        {plugins.length === 0 ? (
          <div className="rounded-lg border border-dashed p-8 text-center">
            <p className="text-muted-foreground">No plugins loaded.</p>
            {canCreate && (
              <p className="text-muted-foreground mt-2 text-sm">
                Add a plugin to enable external account authentication.
              </p>
            )}
          </div>
        ) : (
          <div className="overflow-clip rounded-lg border">
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Plugin</TableHead>
                  <TableHead>Version</TableHead>
                  <TableHead>Auth Type</TableHead>
                  <TableHead>Source</TableHead>
                  <TableHead>Status</TableHead>
                  <TableHead className="w-32" />
                </TableRow>
              </TableHeader>
              <TableBody>
                {plugins.map((plugin) => {
                  const capabilities = visibleCapabilities(plugin.capabilities);
                  return (
                  <TableRow key={plugin.id}>
                    <TableCell>
                      <div className="flex flex-col gap-1">
                        <div className="flex items-center gap-2">
                          <span className="font-medium">{plugin.name}</span>
                          <Badge variant="outline">{plugin.plugin_type}</Badge>
                        </div>
                        <code className="text-muted-foreground text-xs">
                          {plugin.id}
                        </code>
                        {capabilities.length > 0 && (
                          <Collapsible>
                            <CollapsibleTrigger asChild>
                              <button
                                type="button"
                                className="flex w-fit items-center gap-1 text-xs text-muted-foreground hover:text-foreground transition-colors"
                              >
                                <ChevronDown className="size-3" />
                                <span>Permissions</span>
                              </button>
                            </CollapsibleTrigger>
                            <CollapsibleContent className="mt-1 max-w-sm">
                              <PluginCapabilities
                                entries={capabilities}
                                allowedHosts={plugin.allowed_hosts}
                              />
                            </CollapsibleContent>
                          </Collapsible>
                        )}
                      </div>
                    </TableCell>
                    <TableCell>
                      <Badge variant="secondary">{plugin.version}</Badge>
                    </TableCell>
                    <TableCell>
                      <Badge variant="outline">{formatAuthType(plugin.auth_type)}</Badge>
                    </TableCell>
                    <TableCell>
                      <div className="flex items-center gap-1">
                        <Badge variant={plugin.source === "url" ? "default" : "secondary"}>
                          {plugin.source}
                        </Badge>
                        {plugin.url && plugin.source === "url" && (
                          <a
                            href={plugin.url}
                            target="_blank"
                            rel="noopener noreferrer"
                            className="text-muted-foreground hover:text-foreground"
                          >
                            <ExternalLink className="size-3" />
                          </a>
                        )}
                      </div>
                    </TableCell>
                    <TableCell>
                      {plugin.required_secrets?.length === 0 ? (
                        <span className="text-muted-foreground text-sm">No config needed</span>
                      ) : plugin.secrets_configured ? (
                        <div className="flex items-center gap-1.5 text-green-600">
                          <CheckCircle2 className="size-4" />
                          <span className="text-sm">Configured</span>
                        </div>
                      ) : (
                        <div className="flex items-center gap-1.5 text-amber-500">
                          <AlertCircle className="size-4" />
                          <span className="text-sm">Needs config</span>
                        </div>
                      )}
                    </TableCell>
                    <TableCell>
                      <div className="flex gap-1">
                        {canCreate && plugin.update_available && (
                          <Button
                            variant="ghost"
                            size="icon"
                            className="size-8 text-primary"
                            title={`Update to v${plugin.latest_version}`}
                            onClick={() => openUpdateDialog(plugin)}
                          >
                            <ArrowUpCircle className="size-4" />
                          </Button>
                        )}
                        {canCreate && (
                          <Button
                            variant="ghost"
                            size="icon"
                            className="size-8"
                            title="Check for updates"
                            onClick={() => handleCheckUpdate(plugin)}
                            disabled={checkingUpdate === plugin.id}
                          >
                            <Search
                              className={`size-4 ${checkingUpdate === plugin.id ? "animate-spin" : ""}`}
                            />
                          </Button>
                        )}
                        {canCreate && plugin.required_secrets?.length > 0 && (
                          <Button
                            variant="ghost"
                            size="icon"
                            className="size-8"
                            title={encryptionConfigured ? "Configure secrets" : "Encryption not configured"}
                            onClick={() => handleOpenConfig(plugin)}
                            disabled={!encryptionConfigured}
                          >
                            <Settings className="size-4" />
                          </Button>
                        )}
                        {canCreate && plugin.source === "url" && (
                          <Button
                            variant="ghost"
                            size="icon"
                            className="size-8"
                            title="Reload plugin"
                            onClick={() => handleReload(plugin.id)}
                            disabled={reloading === plugin.id}
                          >
                            <RefreshCw
                              className={`size-4 ${reloading === plugin.id ? "animate-spin" : ""}`}
                            />
                          </Button>
                        )}
                        {canDelete && (
                          <Button
                            variant="ghost"
                            size="icon"
                            className="size-8 text-destructive hover:text-destructive"
                            title="Remove plugin"
                            onClick={() => handleRemove(plugin.id)}
                            disabled={removing === plugin.id}
                          >
                            <Trash2 className="size-4" />
                          </Button>
                        )}
                      </div>
                    </TableCell>
                  </TableRow>
                  );
                })}
              </TableBody>
            </Table>
          </div>
        )}

        {/* Configure Secrets Dialog */}
        <ResponsiveDialog open={configOpen} onOpenChange={setConfigOpen}>
          <ResponsiveDialogContent>
            <ResponsiveDialogHeader>
              <ResponsiveDialogTitle>
                Configure {configPlugin?.name}
              </ResponsiveDialogTitle>
              <ResponsiveDialogDescription>
                Enter the required secrets for this plugin. Leave empty to use environment variables.
              </ResponsiveDialogDescription>
            </ResponsiveDialogHeader>
            <div className="grid gap-4 py-4">
              {configPlugin?.required_secrets.map((secret) => (
                <div key={secret.key} className="grid gap-2">
                  <Label htmlFor={`secret-${secret.key}`}>
                    {secret.name}
                    <code className="text-xs font-normal text-muted-foreground ml-2">{secret.key}</code>
                  </Label>
                  {secret.description && (
                    <p className="text-xs text-muted-foreground">{secret.description}</p>
                  )}
                  <Input
                    id={`secret-${secret.key}`}
                    type="password"
                    placeholder="Enter value..."
                    value={secretValues[secret.key] || ""}
                    onChange={(e) =>
                      setSecretValues((prev) => ({ ...prev, [secret.key]: e.target.value }))
                    }
                  />
                </div>
              ))}
            </div>
            <ResponsiveDialogFooter>
              <ResponsiveDialogClose asChild>
                <Button variant="outline" disabled={savingSecrets}>
                  Cancel
                </Button>
              </ResponsiveDialogClose>
              <Button onClick={handleSaveSecrets} disabled={savingSecrets}>
                {savingSecrets ? "Saving..." : "Save Secrets"}
              </Button>
            </ResponsiveDialogFooter>
          </ResponsiveDialogContent>
        </ResponsiveDialog>

        <PluginUpdateDialog
          plugin={updateDialogPlugin}
          open={updateDialogOpen}
          onOpenChange={(open) => {
            setUpdateDialogOpen(open);
            if (!open) setUpdateDialogPlugin(null);
          }}
          onUpdated={() => load()}
        />
      </div>
    </>
  );
}
