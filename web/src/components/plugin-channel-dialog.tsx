"use client";

import { useState } from "react";
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
import {
  ChannelSelect,
  LifecycleNotice,
  unavailableChannelCopy,
} from "@/components/plugin-lifecycle";
import { reloadPlugin, type ChannelEntry, type PluginSummary } from "@/lib/api";

/**
 * Moves a registry install to another channel. The plugin takes that
 * channel's version now and follows it from then on; the server refuses a
 * version that may not be installed, and the installed one keeps running.
 */
export function PluginChannelDialog({
  plugin,
  channels,
  open,
  onOpenChange,
  onSwitched,
}: {
  plugin: PluginSummary;
  /** What each channel points at, when the catalogue holds the project. */
  channels: Record<string, ChannelEntry> | null;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onSwitched: (updated: PluginSummary) => void;
}) {
  const following = plugin.registry?.channel ?? null;
  const [chosen, setChosen] = useState<string | null>(null);
  const [switching, setSwitching] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const channel = chosen ?? following ?? "latest";
  const head = channels?.[channel];
  const unchanged = channel === following;
  const blocked = head !== undefined && !head.installable;

  const close = (next: boolean) => {
    if (!next) {
      setChosen(null);
      setError(null);
    }
    onOpenChange(next);
  };

  const handleSwitch = async () => {
    setSwitching(true);
    setError(null);
    try {
      const updated = await reloadPlugin(plugin.id, { registry: { channel } });
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
              value={channel}
              onValueChange={(next) => {
                setChosen(next);
                setError(null);
              }}
              disabled={switching}
            />
            {channels ? null : (
              <p className="text-xs text-muted-foreground">
                The catalogue doesn&apos;t hold this project, so the version
                each channel points to isn&apos;t known until you switch.
              </p>
            )}
          </div>

          {head && blocked ? (
            <LifecycleNotice
              tone={head.state === "retired" ? "danger" : "warning"}
              title={unavailableChannelCopy(channel, head.state)}
            />
          ) : head?.state === "deprecated" ? (
            <LifecycleNotice
              tone="warning"
              title="This version is deprecated by its publisher"
            >
              It still installs, but its publisher recommends against it.
            </LifecycleNotice>
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
            disabled={switching || unchanged || blocked}
          >
            {switching ? <Loader2 className="animate-spin" /> : null}
            {head
              ? `Switch to ${channel} (v${head.version})`
              : `Switch to ${channel}`}
          </Button>
        </ResponsiveDialogFooter>
      </ResponsiveDialogContent>
    </ResponsiveDialog>
  );
}
