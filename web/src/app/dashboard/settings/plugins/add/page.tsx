"use client";

import { useEffect, useMemo, useState } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { Link2, Search, X } from "lucide-react";

import { useOfficialPlugins } from "@/hooks/use-official-plugins";
import { getPlugins } from "@/lib/api";
import { cataloguePluginHref, urlPluginHref } from "@/lib/plugin-routes";
import { PluginMark } from "@/components/plugin-identity";
import { ReleaseStateBadge } from "@/components/plugin-lifecycle";
import { PageBreadcrumbs } from "@/components/page-breadcrumbs";
import { SiteHeader } from "@/components/site-header";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
} from "@/components/ui/sheet";
import { Skeleton } from "@/components/ui/skeleton";

function isHttpUrl(value: string): boolean {
  try {
    const parsed = new URL(value);
    return parsed.protocol === "http:" || parsed.protocol === "https:";
  } catch {
    return false;
  }
}

export default function AddPluginPage() {
  const router = useRouter();
  // `plugins` says whether the registry answered; `listed` leaves out the
  // retired projects, which can no longer be installed.
  const {
    plugins: everything,
    listed: catalogue,
    loading: catalogueLoading,
  } = useOfficialPlugins();

  const [query, setQuery] = useState("");
  const [urlOpen, setUrlOpen] = useState(false);
  const [url, setUrl] = useState("");
  const [installed, setInstalled] = useState<Set<string>>(new Set());
  const [showInstalled, setShowInstalled] = useState(false);

  useEffect(() => {
    void getPlugins()
      .then((r) => setInstalled(new Set(r.plugins.map((p) => p.id))))
      // An unreadable list only costs the installed badges, so the catalogue
      // still works without it.
      .catch(() => setInstalled(new Set()));
  }, []);

  const results = useMemo(() => {
    const q = query.trim().toLowerCase();
    return catalogue.filter((p) => {
      if (!showInstalled && installed.has(p.id)) return false;
      if (!q) return true;
      return (
        p.name.toLowerCase().includes(q) ||
        p.id.toLowerCase().includes(q) ||
        (p.description?.toLowerCase().includes(q) ?? false)
      );
    });
  }, [catalogue, query, installed, showInstalled]);

  const hiddenCount = showInstalled
    ? 0
    : catalogue.filter((p) => installed.has(p.id)).length;

  return (
    <>
      <SiteHeader title="Add plugin" />
      <PageBreadcrumbs
        crumbs={[
          { label: "Plugins", href: "/dashboard/settings/plugins" },
          { label: "Add plugin" },
        ]}
      />
      <div className="flex flex-1 flex-col gap-4 p-4 md:p-6">
        <div className="flex flex-wrap items-start justify-between gap-3">
          <div className="flex flex-col gap-1">
            <h2 className="text-lg font-semibold">Find a plugin</h2>
            <p className="max-w-prose text-sm text-muted-foreground">
              Plugins published to the HappyProto registry.
            </p>
          </div>
          <Button variant="outline" onClick={() => setUrlOpen(true)}>
            <Link2 />
            Install from URL
          </Button>
        </div>

        <div className="flex flex-wrap items-center gap-4">
          <div className="relative min-w-56 max-w-xl flex-1">
            <Search className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
            <Input
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder="Search plugins"
              aria-label="Search the registry"
              className="pl-9"
              autoFocus
            />
            {query ? (
              <button
                type="button"
                onClick={() => setQuery("")}
                aria-label="Clear search"
                className="absolute right-2 top-1/2 -translate-y-1/2 rounded-sm p-1 text-muted-foreground hover:text-foreground focus-visible:ring-[3px] focus-visible:ring-ring/50 focus-visible:outline-none"
              >
                <X className="size-4" />
              </button>
            ) : null}
          </div>
          <label className="flex items-center gap-2 text-sm">
            <Checkbox
              checked={showInstalled}
              onCheckedChange={(v) => setShowInstalled(v === true)}
            />
            Show installed
            {hiddenCount > 0 ? (
              <span className="text-muted-foreground">
                ({hiddenCount} hidden)
              </span>
            ) : null}
          </label>
        </div>

        {catalogueLoading ? (
          <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
            {[0, 1, 2, 3, 4, 5].map((i) => (
              <Skeleton key={i} className="h-28 rounded-lg" />
            ))}
          </div>
        ) : everything.length === 0 ? (
          <div className="flex max-w-xl flex-col items-start gap-3 rounded-lg border border-dashed p-8">
            <p className="text-sm font-medium">Can&apos;t reach the registry</p>
            <p className="text-sm text-muted-foreground">
              Install from a URL instead.
            </p>
            <Button
              variant="outline"
              size="sm"
              onClick={() => setUrlOpen(true)}
            >
              <Link2 />
              Install from URL
            </Button>
          </div>
        ) : results.length === 0 ? (
          <div className="flex max-w-xl flex-col items-start gap-3 rounded-lg border border-dashed p-8">
            <p className="text-sm text-muted-foreground">
              {query.trim() ? (
                <>
                  No plugin matches{" "}
                  <span className="font-medium text-foreground">
                    {query.trim()}
                  </span>
                  .
                </>
              ) : (
                "No plugin to add."
              )}
            </p>
            <Button
              variant="outline"
              size="sm"
              onClick={() => setUrlOpen(true)}
            >
              <Link2 />
              Install from URL instead
            </Button>
          </div>
        ) : (
          <ul className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
            {results.map((entry) => (
              <li key={entry.id}>
                <Link
                  href={cataloguePluginHref(entry.id)}
                  className="flex h-full w-full flex-col gap-3 rounded-lg border bg-card p-4 text-left shadow-xs transition-colors hover:bg-muted/50 focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50 focus-visible:outline-none"
                >
                  <div className="flex items-start gap-3">
                    <PluginMark
                      name={entry.name}
                      type={entry.plugin_type}
                      iconUrl={entry.icon_url}
                    />
                    <div className="flex min-w-0 flex-1 flex-col gap-0.5">
                      <span className="truncate text-sm font-medium">
                        {entry.name}
                      </span>
                      {entry.latest_version ? (
                        <span className="font-mono text-xs tabular-nums text-muted-foreground">
                          {entry.latest_version}
                        </span>
                      ) : (
                        <span className="text-xs text-muted-foreground">
                          No latest release
                        </span>
                      )}
                    </div>
                    <div className="flex shrink-0 flex-col items-end gap-1">
                      {/* Anyone can publish any slug to the registry, so
                          the badge follows the publisher the entry names and
                          not the catalogue it arrived in. */}
                      {entry.official ? (
                        <Badge variant="secondary">Official</Badge>
                      ) : null}
                      {entry.lifecycle_state === "deprecated" ? (
                        <ReleaseStateBadge state="deprecated" />
                      ) : null}
                      {installed.has(entry.id) ? (
                        <span className="text-xs text-muted-foreground">
                          Installed
                        </span>
                      ) : null}
                    </div>
                  </div>
                  <p className="line-clamp-2 text-sm text-muted-foreground">
                    {entry.description ?? "No description."}
                  </p>
                </Link>
              </li>
            ))}
          </ul>
        )}
      </div>

      <Sheet open={urlOpen} onOpenChange={setUrlOpen}>
        <SheetContent side="right" className="w-full sm:max-w-md">
          <SheetHeader>
            <SheetTitle>Install from URL</SheetTitle>
            <SheetDescription>
              The URL of a plugin&apos;s{" "}
              <code className="font-mono">manifest.json</code>, or the{" "}
              <code className="font-mono">.wasm</code> beside it.
            </SheetDescription>
          </SheetHeader>
          <div className="flex flex-col gap-4 px-4">
            <div className="flex flex-col gap-2">
              <Label htmlFor="plugin-url">Manifest URL</Label>
              <Input
                id="plugin-url"
                value={url}
                onChange={(e) => setUrl(e.target.value)}
                placeholder="https://example.com/my-plugin/manifest.json"
                autoComplete="off"
                spellCheck={false}
                className="font-mono text-xs"
              />
              {url && !isHttpUrl(url) ? (
                <p className="text-xs text-destructive">
                  Needs to be an http or https URL.
                </p>
              ) : null}
            </div>
            <Button
              disabled={!isHttpUrl(url)}
              onClick={() => {
                setUrlOpen(false);
                router.push(urlPluginHref(url));
              }}
            >
              Continue
            </Button>
          </div>
        </SheetContent>
      </Sheet>
    </>
  );
}
