"use client";

import { useEffect, useState } from "react";
import { AlertTriangle, Loader2 } from "lucide-react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { toast } from "sonner";

import {
  ResponsiveDialog,
  ResponsiveDialogContent,
  ResponsiveDialogDescription,
  ResponsiveDialogFooter,
  ResponsiveDialogHeader,
  ResponsiveDialogTitle,
} from "@/components/ui/responsive-dialog";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  previewPlugin,
  reloadPlugin,
  type PluginPreview,
  type PluginSummary,
  type SecretDefinition,
} from "@/lib/api";

interface PluginUpdateDialogProps {
  plugin: PluginSummary | null;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onUpdated: (updated: PluginSummary) => void;
}

export function PluginUpdateDialog({
  plugin,
  open,
  onOpenChange,
  onUpdated,
}: PluginUpdateDialogProps) {
  // The entry the server's update gate approved for this install. The
  // catalogue listing is not consulted: it is one entry per id from any
  // publisher, and may have moved on since the gate ran.
  const manifestUrl = plugin?.latest_manifest_url ?? null;

  const [newManifestPreview, setNewManifestPreview] =
    useState<PluginPreview | null>(null);
  const [previewLoading, setPreviewLoading] = useState(false);
  const [updating, setUpdating] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!open || !plugin) {
      setNewManifestPreview(null);
      setError(null);
      return;
    }
    if (!manifestUrl) {
      setNewManifestPreview(null);
      return;
    }

    let cancelled = false;
    setPreviewLoading(true);
    previewPlugin(manifestUrl)
      .then((preview) => {
        if (!cancelled) setNewManifestPreview(preview);
      })
      .catch(() => {
        if (!cancelled) setNewManifestPreview(null);
      })
      .finally(() => {
        if (!cancelled) setPreviewLoading(false);
      });

    return () => {
      cancelled = true;
    };
  }, [open, plugin, manifestUrl]);

  if (!plugin) return null;

  const installedKeys = new Set(
    plugin.required_secrets.map((secret) => secret.key),
  );
  const newRequiredSecrets: SecretDefinition[] =
    newManifestPreview?.required_secrets.filter(
      (secret) => !installedKeys.has(secret.key),
    ) ?? [];

  const pendingReleases = [...(plugin.pending_releases ?? [])];
  const hasReleaseNotes = pendingReleases.length > 0;
  const hasLatestVersion = Boolean(plugin.latest_version);

  const handleUpdate = async () => {
    if (!manifestUrl) {
      setError("No manifest URL available for this plugin.");
      return;
    }
    setUpdating(true);
    setError(null);
    try {
      const updated = await reloadPlugin(plugin.id, { url: manifestUrl });
      if (plugin.latest_version) {
        try {
          window.localStorage.setItem(
            `happyview:plugin-update-seen:${plugin.id}`,
            plugin.latest_version,
          );
        } catch {
          // ignore storage errors
        }
      }
      onUpdated(updated);
      onOpenChange(false);
      toast.success(
        `Updated ${plugin.name} to v${plugin.latest_version ?? updated.version}`,
      );
    } catch (err) {
      const message =
        err instanceof Error ? err.message : "Failed to update plugin";
      setError(message);
    } finally {
      setUpdating(false);
    }
  };

  const updateDisabled = updating || previewLoading || !manifestUrl;

  return (
    <ResponsiveDialog open={open} onOpenChange={onOpenChange}>
      <ResponsiveDialogContent>
        <ResponsiveDialogHeader>
          <ResponsiveDialogTitle>Update {plugin.name}</ResponsiveDialogTitle>
          <ResponsiveDialogDescription asChild>
            <div className="flex items-center gap-2">
              <Badge variant="outline">v{plugin.version}</Badge>
              <span aria-hidden>→</span>
              <Badge>v{plugin.latest_version ?? "unknown"}</Badge>
            </div>
          </ResponsiveDialogDescription>
        </ResponsiveDialogHeader>

        <div className="flex flex-col gap-4 px-4 md:px-0">
          {newRequiredSecrets.length > 0 && (
            <div className="rounded-lg border border-amber-500/50 bg-amber-500/10 p-4">
              <div className="flex items-start gap-3">
                <AlertTriangle className="mt-0.5 h-5 w-5 shrink-0 text-amber-500" />
                <div className="flex flex-col gap-2">
                  <p className="text-sm font-medium">
                    This update requires new configuration. After updating,
                    you&apos;ll need to set these secrets.
                  </p>
                  <ul className="flex flex-col gap-1 text-sm">
                    {newRequiredSecrets.map((secret) => (
                      <li key={secret.key}>
                        <span className="font-medium">{secret.name}</span>
                        <span className="text-muted-foreground">
                          {" "}
                          ({secret.key})
                        </span>
                      </li>
                    ))}
                  </ul>
                </div>
              </div>
            </div>
          )}

          {hasReleaseNotes ? (
            <div className="max-h-96 overflow-y-auto rounded-md border p-4">
              <div className="flex flex-col gap-6">
                {pendingReleases.map((release) => (
                  <section
                    key={release.version}
                    className="flex flex-col gap-2"
                  >
                    <h3 className="text-sm font-semibold">
                      v{release.version} ·{" "}
                      {new Date(release.published_at).toLocaleDateString()}
                    </h3>
                    <div className="prose prose-sm dark:prose-invert max-w-none">
                      <ReactMarkdown remarkPlugins={[remarkGfm]}>
                        {release.body}
                      </ReactMarkdown>
                    </div>
                  </section>
                ))}
              </div>
            </div>
          ) : (
            <p className="text-sm text-muted-foreground">
              No release notes available.
            </p>
          )}

          {error && (
            <div className="rounded-lg border border-destructive/50 bg-destructive/10 p-3 text-sm text-destructive">
              {error}
            </div>
          )}
        </div>

        <ResponsiveDialogFooter>
          <Button
            variant="outline"
            onClick={() => onOpenChange(false)}
            disabled={updating}
          >
            Cancel
          </Button>
          <Button onClick={handleUpdate} disabled={updateDisabled}>
            {updating && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
            {hasLatestVersion
              ? `Update to v${plugin.latest_version}`
              : "Update"}
          </Button>
        </ResponsiveDialogFooter>
      </ResponsiveDialogContent>
    </ResponsiveDialog>
  );
}
