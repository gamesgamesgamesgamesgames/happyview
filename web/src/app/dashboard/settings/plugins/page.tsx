"use client";

import { useCallback, useEffect, useMemo, useState } from "react";
import Link from "next/link";
import {
  AlertTriangle,
  ArrowUpCircle,
  Blocks,
  Plus,
  Search,
} from "lucide-react";

import { useCurrentUser } from "@/hooks/use-current-user";
import { getPlugins } from "@/lib/api";
import { installedPluginHref } from "@/lib/plugin-routes";
import type { PluginSummary, PluginType } from "@/types/plugins";
import {
  PluginMark,
  TYPE_LABEL,
  TYPE_NOTE,
} from "@/components/plugin-identity";
import { SiteHeader } from "@/components/site-header";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";

type TypeFilter = PluginType | "all";
type SortKey = "attention" | "name" | "version";

const TYPE_ORDER: PluginType[] = ["library", "interpreter", "auth"];

/**
 * What the row says at a glance. A plugin that needs something is the only
 * state worth colour, so a healthy instance reads calm rather than as a wall
 * of green ticks.
 */
function attention(plugin: PluginSummary): {
  needs: boolean;
  label: string | null;
  kind: "config" | "update" | null;
} {
  if (!plugin.secrets_configured) {
    return { needs: true, label: "Needs configuration", kind: "config" };
  }
  if (plugin.update_available) {
    return { needs: true, label: "Update available", kind: "update" };
  }
  return { needs: false, label: null, kind: null };
}

export default function PluginsPage() {
  const { hasPermission } = useCurrentUser();
  const [plugins, setPlugins] = useState<PluginSummary[]>([]);
  const [encryptionConfigured, setEncryptionConfigured] = useState(true);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const [query, setQuery] = useState("");
  const [typeFilter, setTypeFilter] = useState<TypeFilter>("all");
  const [sort, setSort] = useState<SortKey>("attention");
  const [grouped, setGrouped] = useState(true);

  const canManage = hasPermission("plugins:manage");

  const load = useCallback(async () => {
    try {
      const response = await getPlugins();
      setPlugins(response.plugins);
      setEncryptionConfigured(response.encryption_configured);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const visible = useMemo(() => {
    const q = query.trim().toLowerCase();
    const matches = plugins.filter((plugin) => {
      if (typeFilter !== "all" && plugin.plugin_type !== typeFilter)
        return false;
      if (!q) return true;
      // What an operator would actually type: the name they see, the id they
      // installed, the namespace a script requires, or words from the blurb.
      return (
        plugin.name.toLowerCase().includes(q) ||
        plugin.id.toLowerCase().includes(q) ||
        (plugin.namespace?.toLowerCase().includes(q) ?? false) ||
        (plugin.description?.toLowerCase().includes(q) ?? false)
      );
    });

    const byName = (a: PluginSummary, b: PluginSummary) =>
      a.name.localeCompare(b.name);

    return [...matches].sort((a, b) => {
      if (sort === "name") return byName(a, b);
      if (sort === "version") return a.version.localeCompare(b.version);
      const needs = Number(attention(b).needs) - Number(attention(a).needs);
      return needs !== 0 ? needs : byName(a, b);
    });
  }, [plugins, query, typeFilter, sort]);

  const sections = useMemo(() => {
    if (!grouped) return [{ type: null as PluginType | null, rows: visible }];
    return TYPE_ORDER.map((type) => ({
      type,
      rows: visible.filter((p) => p.plugin_type === type),
    })).filter((section) => section.rows.length > 0);
  }, [visible, grouped]);

  const needing = plugins.filter((p) => attention(p).needs).length;

  return (
    <>
      <SiteHeader title="Plugins" />
      <div className="flex flex-1 flex-col gap-4 p-4 md:p-6">
        {error ? (
          <p role="alert" className="text-sm text-destructive">
            {error}
          </p>
        ) : null}

        {!encryptionConfigured ? (
          <div className="flex items-start gap-3 rounded-lg border bg-muted/40 p-4">
            <AlertTriangle className="mt-0.5 size-5 shrink-0 text-muted-foreground" />
            <div className="flex flex-col gap-1">
              <p className="text-sm font-medium">
                Plugin secrets cannot be stored
              </p>
              <p className="text-sm text-muted-foreground">
                Set{" "}
                <code className="rounded bg-muted px-1 py-0.5 font-mono text-xs">
                  TOKEN_ENCRYPTION_KEY
                </code>{" "}
                to a base64-encoded 32-byte key and restart. Generate one with{" "}
                <code className="rounded bg-muted px-1 py-0.5 font-mono text-xs">
                  openssl rand -base64 32
                </code>
                .
              </p>
            </div>
          </div>
        ) : null}

        <div className="flex flex-wrap items-start justify-between gap-3">
          <div className="flex flex-col gap-1">
            <h2 className="text-lg font-semibold">Installed plugins</h2>
            <p className="text-sm text-muted-foreground">
              {loading
                ? "Loading"
                : plugins.length === 0
                  ? "Nothing installed yet."
                  : needing > 0
                    ? `${plugins.length} installed · ${needing} need attention`
                    : `${plugins.length} installed · all configured`}
            </p>
          </div>
          {canManage ? (
            <Button asChild>
              <Link href="/dashboard/settings/plugins/add">
                <Plus />
                Add plugin
              </Link>
            </Button>
          ) : null}
        </div>

        {plugins.length > 0 ? (
          <div className="flex flex-wrap items-center gap-2">
            <div className="relative min-w-56 flex-1">
              <Search className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
              <Input
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                placeholder="Search by name, id or namespace"
                aria-label="Search installed plugins"
                className="pl-9"
              />
            </div>
            <Select
              value={typeFilter}
              onValueChange={(v) => setTypeFilter(v as TypeFilter)}
            >
              <SelectTrigger className="w-40" aria-label="Filter by kind">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="all">All kinds</SelectItem>
                {TYPE_ORDER.map((type) => (
                  <SelectItem key={type} value={type}>
                    {TYPE_LABEL[type]}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <Select value={sort} onValueChange={(v) => setSort(v as SortKey)}>
              <SelectTrigger className="w-48" aria-label="Sort plugins">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="attention">Needs attention first</SelectItem>
                <SelectItem value="name">Name</SelectItem>
                <SelectItem value="version">Version</SelectItem>
              </SelectContent>
            </Select>
            <Button
              variant={grouped ? "secondary" : "outline"}
              onClick={() => setGrouped((g) => !g)}
              aria-pressed={grouped}
            >
              Group by kind
            </Button>
          </div>
        ) : null}

        {loading ? (
          <div className="flex flex-col gap-2">
            {[0, 1, 2].map((i) => (
              <Skeleton key={i} className="h-[66px] w-full rounded-lg" />
            ))}
          </div>
        ) : plugins.length === 0 ? (
          <div className="flex flex-col items-start gap-3 rounded-lg border border-dashed p-8">
            <Blocks className="size-5 text-muted-foreground" />
            <div className="flex flex-col gap-1">
              <p className="text-sm font-medium">No plugins installed</p>
              <p className="max-w-prose text-sm text-muted-foreground">
                Libraries your scripts can call, interpreters that run them, and
                sign-in providers.
              </p>
            </div>
            {canManage ? (
              <Button asChild size="sm">
                <Link href="/dashboard/settings/plugins/add">
                  <Plus />
                  Add plugin
                </Link>
              </Button>
            ) : null}
          </div>
        ) : visible.length === 0 ? (
          <div className="flex flex-col items-start gap-3 rounded-lg border border-dashed p-8">
            <p className="text-sm text-muted-foreground">
              No plugin matches this search.
            </p>
            <Button
              variant="outline"
              size="sm"
              onClick={() => {
                setQuery("");
                setTypeFilter("all");
              }}
            >
              Clear filters
            </Button>
          </div>
        ) : (
          <div className="flex flex-col gap-6">
            {sections.map((section) => (
              <div key={section.type ?? "all"} className="flex flex-col gap-2">
                {section.type ? (
                  <div className="flex items-baseline gap-2">
                    <h3 className="text-sm font-medium">
                      {TYPE_LABEL[section.type]}
                    </h3>
                    <span className="text-xs text-muted-foreground">
                      {TYPE_NOTE[section.type]}
                    </span>
                  </div>
                ) : null}
                <ul className="flex flex-col gap-2">
                  {section.rows.map((plugin) => {
                    const state = attention(plugin);
                    return (
                      <li key={plugin.id}>
                        <Link
                          href={installedPluginHref(plugin.id)}
                          aria-label={`${plugin.name}, ${TYPE_LABEL[plugin.plugin_type]}${
                            state.label ? `, ${state.label}` : ""
                          }`}
                          className="flex w-full items-center gap-4 rounded-lg border bg-card p-3 text-left shadow-xs transition-colors hover:bg-muted/50 focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50 focus-visible:outline-none"
                        >
                          <PluginMark
                            name={plugin.name}
                            type={plugin.plugin_type}
                            iconUrl={plugin.icon_url}
                          />
                          <div className="flex min-w-0 shrink-0 basis-56 flex-col gap-0.5">
                            <span className="truncate text-sm font-medium">
                              {plugin.name}
                            </span>
                            <span className="truncate font-mono text-xs text-muted-foreground">
                              {plugin.namespace ?? plugin.id}
                            </span>
                          </div>
                          {/* The middle earns its width only when there is
                              something to put there; an absent description
                              leaves the row short rather than padded out. */}
                          <span className="hidden min-w-0 flex-1 truncate text-sm text-muted-foreground lg:block">
                            {plugin.description ?? ""}
                          </span>
                          <span className="flex-1 lg:hidden" />
                          {!grouped ? (
                            <Badge
                              variant="outline"
                              className="hidden shrink-0 sm:inline-flex"
                            >
                              {TYPE_LABEL[plugin.plugin_type]}
                            </Badge>
                          ) : null}
                          {state.needs ? (
                            <Badge variant="destructive" className="shrink-0">
                              {state.kind === "update" ? (
                                <ArrowUpCircle />
                              ) : (
                                <AlertTriangle />
                              )}
                              <span className="hidden sm:inline">
                                {state.label}
                              </span>
                            </Badge>
                          ) : null}
                          <span className="shrink-0 font-mono text-xs tabular-nums text-muted-foreground">
                            {plugin.version}
                          </span>
                        </Link>
                      </li>
                    );
                  })}
                </ul>
              </div>
            ))}
          </div>
        )}
      </div>
    </>
  );
}
