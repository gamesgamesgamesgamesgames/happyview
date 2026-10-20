"use client";

import { useState } from "react";
import { KeyRound, Package, Terminal } from "lucide-react";

import { cn } from "@/lib/utils";
import type { PluginType } from "@/types/plugins";

// A plugin's icon and banner are each independently optional, so there are
// four cases and none of them may look like a failure to load. Solved once
// here: the ledger row, the detail sheet and the registry results all read
// the same component, so they cannot disagree about what "no image" looks
// like.

const TYPE_ICON: Record<PluginType, typeof Package> = {
  library: Package,
  interpreter: Terminal,
  auth: KeyRound,
};

export const TYPE_LABEL: Record<PluginType, string> = {
  library: "Library",
  interpreter: "Interpreter",
  auth: "Auth provider",
};

/** What a plugin kind is for, in the operator's terms. */
export const TYPE_NOTE: Record<PluginType, string> = {
  library: "Called from scripts",
  interpreter: "Runs scripts",
  auth: "External sign-in",
};

export function PluginTypeIcon({
  type,
  className,
}: {
  type: PluginType;
  className?: string;
}) {
  const Icon = TYPE_ICON[type];
  return <Icon aria-hidden className={cn("size-4", className)} />;
}

/**
 * The square mark. An icon when the plugin ships one, the kind's glyph when it
 * does not — never a blank tile, which reads as a broken image.
 */
export function PluginMark({
  name,
  type,
  iconUrl,
  size = "row",
  className,
}: {
  name: string;
  type: PluginType;
  iconUrl: string | null;
  size?: "row" | "sheet";
  className?: string;
}) {
  const [failed, setFailed] = useState(false);
  const box = size === "row" ? "size-10" : "size-14";
  const showImage = iconUrl && !failed;

  return (
    <div
      className={cn(
        box,
        "flex shrink-0 items-center justify-center overflow-hidden rounded-md",
        // Artwork stands on its own; only the fallback glyph needs a tile
        // under it, or it floats unanchored beside the name.
        showImage ? "" : "border bg-muted/50",
        className,
      )}
    >
      {showImage ? (
        // eslint-disable-next-line @next/next/no-img-element
        <img
          src={iconUrl}
          alt=""
          className="size-full object-contain"
          onError={() => setFailed(true)}
        />
      ) : (
        <PluginTypeIcon
          type={type}
          className={cn("text-muted-foreground", size === "sheet" && "size-6")}
        />
      )}
      <span className="sr-only">{name}</span>
    </div>
  );
}

/**
 * The sheet's header image, rendered only when the plugin ships a banner. A
 * 3:1 field holding nothing but a small glyph is 160px of apology, so the
 * absent case renders nothing at all and the sheet's own header carries the
 * mark instead.
 */
export function PluginBanner({ bannerUrl }: { bannerUrl: string | null }) {
  const [failed, setFailed] = useState(false);
  if (!bannerUrl || failed) return null;

  return (
    <div className="relative aspect-[3/1] w-full overflow-hidden rounded-lg border bg-muted/50">
      {/* eslint-disable-next-line @next/next/no-img-element */}
      <img
        src={bannerUrl}
        alt=""
        className="size-full object-cover"
        onError={() => setFailed(true)}
      />
    </div>
  );
}
