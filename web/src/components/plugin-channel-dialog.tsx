"use client";

import { useEffect, useState } from "react";
import { Loader2 } from "lucide-react";
import { toast } from "sonner";

import {
  ResponsiveDialog,
  ResponsiveDialogContent,
  ResponsiveDialogDescription,
  ResponsiveDialogFooter,
  ResponsiveDialogHeader,
  ResponsiveDialogTitle,
} from "@/components/ui/responsive-dialog";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import { Skeleton } from "@/components/ui/skeleton";
import {
  ChannelSelect,
  LifecycleNotice,
  headInstallable,
  unavailableChannelCopy,
} from "@/components/plugin-lifecycle";
import {
  CapabilityConsent,
  newCapabilities,
  visibleCapabilities,
} from "@/components/plugin-capabilities";
import {
  previewPlugin,
  reloadPlugin,
  type ChannelEntry,
  type PluginPreview,
  type PluginSummary,
  type ReleaseEntry,
} from "@/lib/api";

/** A preview and the channel it was read for, so an answer for a channel no
 * longer chosen is never shown as the current one. */
type Reading = {
  channel: string;
  preview: PluginPreview | null;
  error: string | null;
};

/**
 * Moves a registry install to another channel. The plugin takes that
 * channel's version now and follows it from then on. The version is read
 * first so the operator sees, and agrees to, any permission it adds; the
 * server refuses a version that may not be installed, and the installed one
 * keeps running.
 */
export function PluginChannelDialog({
  plugin,
  channels,
  releases,
  open,
  onOpenChange,
  onSwitched,
}: {
  plugin: PluginSummary;
  /** What each channel points at, when the catalogue holds the project. */
  channels: Record<string, ChannelEntry> | null;
  /** The project's releases, when the catalogue holds the project. */
  releases: ReleaseEntry[];
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onSwitched: (updated: PluginSummary) => void;
}) {
  const did = plugin.registry?.did ?? null;
  const name = plugin.registry?.name ?? null;
  const following = plugin.registry?.channel ?? null;
  const [chosen, setChosen] = useState<string | null>(null);
  const [reading, setReading] = useState<Reading | null>(null);
  const [acceptedFor, setAcceptedFor] = useState<string | null>(null);
  const [switching, setSwitching] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Preselect what the install follows (or `latest` for a pinned install),
  // but only when the catalogue offers it: a value the list doesn't hold
  // would read as chosen while naming nothing.
  const offered = (c: string) => !channels || c in channels;
  const fallback = following ?? "latest";
  const channel = chosen ?? (offered(fallback) ? fallback : null);
  const head = channel ? channels?.[channel] : undefined;
  const unchanged = channel === following;
  const blocked = head !== undefined && !headInstallable(head, releases);
  const headRelease = head
    ? releases.find((r) => r.version === head.version)
    : undefined;

  // Read the chosen channel's version whenever it could be switched to.
  const target =
    open && channel && !unchanged && !blocked && did && name ? channel : null;

  useEffect(() => {
    if (!target || !did || !name) return;
    let cancelled = false;
    void previewPlugin({ registry: { did, name, channel: target } })
      .then((preview) => {
        if (!cancelled) setReading({ channel: target, preview, error: null });
      })
      .catch((e: unknown) => {
        if (cancelled) return;
        setReading({
          channel: target,
          preview: null,
          error: e instanceof Error ? e.message : String(e),
        });
      });
    return () => {
      cancelled = true;
    };
  }, [target, did, name]);

  const current = target && reading?.channel === target ? reading : null;
  const preview = current?.preview ?? null;
  const loadingPreview = target !== null && current === null;
  const added = preview
    ? newCapabilities(plugin.capabilities, preview.capabilities)
    : [];
  const accepted = acceptedFor !== null && acceptedFor === target;
  const consented = added.length === 0 || accepted;

  const close = (next: boolean) => {
    if (!next) {
      setChosen(null);
      setAcceptedFor(null);
      setError(null);
    }
    onOpenChange(next);
  };

  const handleSwitch = async () => {
    if (!channel || !preview || !consented) return;
    setSwitching(true);
    setError(null);
    try {
      const updated = await reloadPlugin(plugin.id, {
        registry: { channel },
        // Everything the version declares, so the server refuses one that
        // would be granted more than this dialog showed.
        accepted_capabilities: visibleCapabilities(preview.capabilities).map(
          (c) => c.name,
        ),
      });
      onSwitched(updated);
      close(false);
      toast.success(
        `${plugin.name} now follows ${channel} at v${updated.version}`,
      );
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSwitching(false);
    }
  };

  const version = head?.version ?? preview?.version;

  return (
    <ResponsiveDialog open={open} onOpenChange={close}>
      <ResponsiveDialogContent>
        <ResponsiveDialogHeader>
          <ResponsiveDialogTitle>Switch channel</ResponsiveDialogTitle>
          <ResponsiveDialogDescription>
            {following
              ? `${plugin.name} follows the ${following} channel.`
              : `${plugin.name} is pinned to v${plugin.version}.`}{" "}
            The plugin installs the chosen channel&apos;s version now and takes
            its updates from then on.
          </ResponsiveDialogDescription>
        </ResponsiveDialogHeader>

        <div className="flex flex-col gap-4 px-4 md:px-0">
          <div className="flex flex-col gap-2">
            <Label htmlFor="switch-channel">Channel</Label>
            <ChannelSelect
              id="switch-channel"
              channels={channels}
              releases={releases}
              value={channel ?? ""}
              onValueChange={(next) => {
                setChosen(next);
                setAcceptedFor(null);
                setError(null);
              }}
              disabled={switching}
            />
            {channels ? null : (
              <p className="text-xs text-muted-foreground">
                The catalogue doesn&apos;t hold this project, so the version
                each channel points to is read when you choose one.
              </p>
            )}
          </div>

          {head && blocked ? (
            <LifecycleNotice
              tone={head.state === "retired" ? "danger" : "warning"}
              title={unavailableChannelCopy(channel ?? "", head.state)}
            >
              {headRelease?.reason ?? null}
            </LifecycleNotice>
          ) : head?.state === "deprecated" ? (
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
          ) : null}

          {loadingPreview ? (
            <Skeleton className="h-16 w-full rounded-lg" />
          ) : null}

          {preview && target ? (
            <CapabilityConsent
              id="switch-capabilities-consent"
              entries={added}
              allowedHosts={preview.allowed_hosts}
              accepted={accepted}
              onAcceptedChange={(v) => setAcceptedFor(v ? target : null)}
              disabled={switching}
            />
          ) : null}

          {current?.error ? (
            <p
              role="alert"
              className="rounded-lg border border-destructive/50 bg-destructive/10 p-3 text-sm text-destructive"
            >
              Couldn&apos;t read that channel&apos;s version, so its permissions
              can&apos;t be shown: {current.error}
            </p>
          ) : null}

          {error ? (
            <p
              role="alert"
              className="rounded-lg border border-destructive/50 bg-destructive/10 p-3 text-sm text-destructive"
            >
              {error}
            </p>
          ) : null}
        </div>

        <ResponsiveDialogFooter>
          <Button
            variant="outline"
            onClick={() => close(false)}
            disabled={switching}
          >
            Cancel
          </Button>
          <Button
            onClick={() => void handleSwitch()}
            disabled={
              switching ||
              !channel ||
              unchanged ||
              blocked ||
              !preview ||
              !consented
            }
          >
            {switching ? <Loader2 className="animate-spin" /> : null}
            {!channel
              ? "Switch"
              : version
                ? `Switch to ${channel} (v${version})`
                : `Switch to ${channel}`}
          </Button>
        </ResponsiveDialogFooter>
      </ResponsiveDialogContent>
    </ResponsiveDialog>
  );
}
