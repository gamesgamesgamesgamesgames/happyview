"use client";

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  type ColumnDef,
  type ColumnFiltersState,
  type VisibilityState,
  getCoreRowModel,
  useReactTable,
} from "@tanstack/react-table";

import { getEvents, getGrantReads, type EventLogEntry } from "@/lib/api";
import { DataTable } from "@/components/data-table/data-table";
import { DataTableColumnHeader } from "@/components/data-table/data-table-column-header";
import { DataTableToolbar } from "@/components/data-table/data-table-toolbar";
import { CodeBlock } from "@/components/code-block";
import { SiteHeader } from "@/components/site-header";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  ResponsiveDialog,
  ResponsiveDialogContent,
  ResponsiveDialogHeader,
  ResponsiveDialogTitle,
} from "@/components/ui/responsive-dialog";
import { ChevronLeft, ChevronRight } from "lucide-react";

function severityBadge(severity: string) {
  switch (severity) {
    case "error":
      return <Badge variant="destructive">error</Badge>;
    case "warn":
      return (
        <Badge className="bg-amber-500/15 text-amber-700 dark:text-amber-400 hover:bg-amber-500/25 border-amber-500/20">
          warn
        </Badge>
      );
    default:
      return <Badge variant="secondary">info</Badge>;
  }
}

function timeAgo(dateStr: string): string {
  const now = Date.now();
  const then = new Date(dateStr).getTime();
  const seconds = Math.floor((now - then) / 1000);
  if (seconds < 60) return `${seconds}s ago`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.floor(hours / 24);
  return `${days}d ago`;
}

/** Known detail keys that get their own sections — excluded from the "Other" dump. */
const KNOWN_KEYS = [
  "error",
  "errorType",
  "method",
  "line",
  "input",
  "params",
  "response",
  "script_source",
  "caller_did",
  "duration_ms",
  "response_size",
  "message",
  "level",
  "grant_id",
  "reason",
  "uris",
] as const;

function GrantReads({ grantId }: { grantId: string }) {
  const [reads, setReads] = useState<EventLogEntry[] | null>(null);

  useEffect(() => {
    let cancelled = false;
    getGrantReads(grantId)
      .then((r) => {
        if (!cancelled) setReads(r.events);
      })
      .catch(() => {
        if (!cancelled) setReads([]);
      });
    return () => {
      cancelled = true;
    };
  }, [grantId]);

  return (
    <div className="flex flex-col gap-2">
      <span className="text-muted-foreground text-sm">Reads under this grant</span>
      {reads === null ? (
        <p className="text-muted-foreground text-xs">Loading…</p>
      ) : reads.length === 0 ? (
        <p className="text-muted-foreground text-xs">Nothing was read.</p>
      ) : (
        <ul className="flex flex-col gap-2">
          {reads.map((r) => (
            <li key={r.id} className="rounded border p-2 text-xs">
              <div className="flex justify-between gap-2">
                <span className="font-mono">{String(r.detail.action)}</span>
                <span className="text-muted-foreground">
                  {new Date(r.created_at).toLocaleString()}
                </span>
              </div>
              {Array.isArray(r.detail.uris) && (
                <ul className="mt-1 font-mono break-all">
                  {(r.detail.uris as string[]).map((u) => (
                    <li key={u}>{u}</li>
                  ))}
                </ul>
              )}
              {typeof r.detail.cid === "string" && (
                <p className="mt-1 font-mono break-all">blob {r.detail.cid}</p>
              )}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function EventDetailBody({ event }: { event: EventLogEntry }) {
  const d = event.detail;

  // Collect any keys not in KNOWN_KEYS for the "Other" section
  const otherKeys = Object.keys(d).filter(
    (k) => !(KNOWN_KEYS as readonly string[]).includes(k),
  );
  const otherDetail =
    otherKeys.length > 0
      ? Object.fromEntries(otherKeys.map((k) => [k, d[k]]))
      : null;

  return (
    <div className="flex flex-col gap-4">
      {/* Metadata grid */}
      <div className="grid grid-cols-2 gap-4 text-sm">
        <div>
          <span className="text-muted-foreground">Subject</span>
          <p className="font-mono text-xs">{event.subject ?? "--"}</p>
        </div>
        <div>
          <span className="text-muted-foreground">Actor</span>
          <p className="font-mono text-xs">
            {event.actor_did ?? "System"}
          </p>
        </div>
        <div>
          <span className="text-muted-foreground">Time</span>
          <p className="text-xs">
            {new Date(event.created_at).toLocaleString()}
          </p>
        </div>
        {d.method != null && (
          <div>
            <span className="text-muted-foreground">Method</span>
            <p className="font-mono text-xs">{String(d.method)}</p>
          </div>
        )}
        {d.caller_did != null && (
          <div>
            <span className="text-muted-foreground">Caller DID</span>
            <p className="font-mono text-xs">{String(d.caller_did)}</p>
          </div>
        )}
        {d.duration_ms != null && (
          <div>
            <span className="text-muted-foreground">Duration</span>
            <p className="text-xs tabular-nums">{String(d.duration_ms)}ms</p>
          </div>
        )}
        {d.response_size != null && (
          <div>
            <span className="text-muted-foreground">Response Size</span>
            <p className="text-xs tabular-nums">
              {Number(d.response_size).toLocaleString()} bytes
            </p>
          </div>
        )}
      </div>

      {/* Access grant */}
      {event.event_type === "space.access_granted" &&
        typeof d.grant_id === "string" && (
          <>
            <div>
              <span className="text-muted-foreground text-sm">Reason</span>
              <p className="text-sm break-words">{String(d.reason)}</p>
            </div>
            <GrantReads grantId={d.grant_id} />
          </>
        )}
      {event.event_type === "space.moderator_read" &&
        typeof d.grant_id === "string" && (
          <div>
            <span className="text-muted-foreground text-sm">Grant</span>
            <p className="font-mono text-xs">{d.grant_id}</p>
          </div>
        )}

      {/* Plugin log message */}
      {d.message != null && (
        <div>
          <div className="flex items-center gap-2">
            <span className="text-muted-foreground text-sm">Message</span>
            {d.level != null && (
              <Badge variant="outline" className="text-xs uppercase">
                {String(d.level)}
              </Badge>
            )}
          </div>
          <p className="mt-1 whitespace-pre-wrap break-words font-mono text-xs">
            {String(d.message)}
          </p>
        </div>
      )}

      {/* Error section */}
      {d.error != null && (
        <div>
          <span className="text-muted-foreground text-sm">Error</span>
          <div className="bg-destructive/10 text-destructive mt-1 rounded-md p-3 font-mono text-xs">
            {d.errorType != null && (
              <Badge variant="destructive" className="mb-2 mr-2">
                {String(d.errorType)}
              </Badge>
            )}
            {d.line != null && (
              <Badge variant="outline" className="mb-2">
                line {String(d.line)}
              </Badge>
            )}
            <p className="mt-1">{String(d.error)}</p>
          </div>
        </div>
      )}

      {/* Request input/params */}
      {(d.input != null || d.params != null) && (
        <div>
          <span className="text-muted-foreground text-sm">
            {d.input != null ? "Request Input" : "Request Params"}
          </span>
          <CodeBlock
            code={JSON.stringify(d.input ?? d.params, null, 2)}
            className="mt-1 max-h-64 rounded-md"
          />
        </div>
      )}

      {/* Response */}
      {d.response != null && (
        <div>
          <span className="text-muted-foreground text-sm">Response</span>
          <CodeBlock
            code={JSON.stringify(d.response, null, 2)}
            className="mt-1 max-h-96 rounded-md"
          />
        </div>
      )}

      {/* Script source */}
      {d.script_source != null && (
        <div>
          <span className="text-muted-foreground text-sm">Script Source</span>
          <CodeBlock
            code={String(d.script_source)}
            lang="lua"
            className="mt-1 max-h-64 rounded-md"
          />
        </div>
      )}

      {/* Other detail fields */}
      {otherDetail && (
        <div>
          <span className="text-muted-foreground text-sm">Other</span>
          <CodeBlock
            code={JSON.stringify(otherDetail, null, 2)}
            className="mt-1 rounded-md"
          />
        </div>
      )}
    </div>
  );
}

export default function EventsPage() {
  const [events, setEvents] = useState<EventLogEntry[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [viewEvent, setViewEvent] = useState<EventLogEntry | null>(null);

  // Pagination
  const [cursorStack, setCursorStack] = useState<string[]>([]);
  const [nextCursor, setNextCursor] = useState<string | null>(null);

  // Filters — driven by TanStack column filter state
  const [columnFilters, setColumnFilters] = useState<ColumnFiltersState>([]);
  const [columnVisibility, setColumnVisibility] = useState<VisibilityState>({});

  // Debounce subject filter to avoid firing on every keystroke
  const debounceRef = useRef<ReturnType<typeof setTimeout>>(null);
  const [debouncedFilters, setDebouncedFilters] =
    useState<ColumnFiltersState>(columnFilters);

  useEffect(() => {
    const subjectFilter = columnFilters.find((f) => f.id === "subject");
    const prevSubjectFilter = debouncedFilters.find((f) => f.id === "subject");
    const subjectChanged = subjectFilter?.value !== prevSubjectFilter?.value;

    if (subjectChanged) {
      if (debounceRef.current) clearTimeout(debounceRef.current);
      debounceRef.current = setTimeout(() => {
        setDebouncedFilters(columnFilters);
      }, 300);
    } else {
      setDebouncedFilters(columnFilters);
    }
  }, [columnFilters]);

  const fetchEvents = useCallback(
    async (cursor?: string) => {
      setLoading(true);
      setError(null);
      try {
        const categoryFilter = debouncedFilters.find(
          (f) => f.id === "event_type",
        )?.value as string[] | undefined;
        const severityFilter = debouncedFilters.find((f) => f.id === "severity")
          ?.value as string[] | undefined;
        const subjectFilter = debouncedFilters.find((f) => f.id === "subject")
          ?.value as string | undefined;

        const data = await getEvents({
          category: categoryFilter?.length ? categoryFilter.join(",") : undefined,
          severity: severityFilter?.length ? severityFilter.join(",") : undefined,
          subject: subjectFilter || undefined,
          cursor,
          limit: 50,
        });
        setEvents(data.events);
        setNextCursor(data.cursor);
      } catch (e: unknown) {
        setError(e instanceof Error ? e.message : String(e));
        setEvents([]);
        setNextCursor(null);
      } finally {
        setLoading(false);
      }
    },
    [debouncedFilters],
  );

  // Fetch on mount and when filters change (reset to first page)
  useEffect(() => {
    setCursorStack([]);
    fetchEvents();
  }, [fetchEvents]);

  // Auto-refresh every 5s when on first page
  useEffect(() => {
    if (cursorStack.length > 0) return;
    const interval = setInterval(() => fetchEvents(), 5000);
    return () => clearInterval(interval);
  }, [fetchEvents, cursorStack.length]);

  function handleNext() {
    if (!nextCursor) return;
    setCursorStack((prev) => [...prev, nextCursor]);
    fetchEvents(nextCursor);
  }

  function handlePrevious() {
    if (cursorStack.length === 0) return;
    const stack = [...cursorStack];
    stack.pop();
    const prevCursor = stack.length > 0 ? stack[stack.length - 1] : undefined;
    setCursorStack(stack);
    fetchEvents(prevCursor);
  }

  const columns = useMemo<ColumnDef<EventLogEntry>[]>(
    () => [
      {
        id: "subject",
        accessorKey: "subject",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} label="Subject" />
        ),
        cell: ({ row }) => (
          <span
            className="font-mono text-xs block max-w-xs truncate"
            title={row.original.subject ?? ""}
          >
            {row.original.subject ?? "--"}
          </span>
        ),
        enableColumnFilter: true,
        enableSorting: false,
        meta: {
          label: "Subject",
          placeholder: "Filter by subject...",
          variant: "text",
        },
      },
      {
        id: "severity",
        accessorKey: "severity",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} label="Severity" />
        ),
        cell: ({ row }) => severityBadge(row.original.severity),
        enableColumnFilter: true,
        enableSorting: false,
        enableHiding: false,
        meta: {
          label: "Severity",
          variant: "select",
          options: [
            { label: "Info", value: "info" },
            { label: "Warn", value: "warn" },
            { label: "Error", value: "error" },
          ],
        },
      },
      {
        id: "event_type",
        accessorKey: "event_type",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} label="Event Type" />
        ),
        cell: ({ row }) => (
          <span className="font-mono text-sm">{row.original.event_type}</span>
        ),
        enableColumnFilter: true,
        enableSorting: false,
        meta: {
          label: "Category",
          variant: "select",
          options: [
            { label: "Lexicon", value: "lexicon" },
            { label: "Record", value: "record" },
            { label: "Script", value: "script" },
            { label: "Admin", value: "admin" },
            { label: "Backfill", value: "backfill" },
            { label: "Plugin", value: "plugin" },
          ],
        },
      },
      {
        id: "actor_did",
        accessorKey: "actor_did",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} label="Actor" />
        ),
        cell: ({ row }) => (
          <span
            className="font-mono text-xs block max-w-[200px] truncate"
            title={row.original.actor_did ?? "System"}
          >
            {row.original.actor_did ?? "System"}
          </span>
        ),
        enableSorting: false,
      },
      {
        id: "created_at",
        accessorKey: "created_at",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} label="Time" />
        ),
        cell: ({ row }) => (
          <span
            className="text-muted-foreground whitespace-nowrap text-sm tabular-nums"
            title={new Date(row.original.created_at).toLocaleString()}
          >
            {timeAgo(row.original.created_at)}
          </span>
        ),
        enableSorting: false,
      },
    ],
    [],
  );

  const table = useReactTable({
    data: events,
    columns,
    state: { columnFilters, columnVisibility },
    defaultColumn: {
      enableColumnFilter: false,
    },
    onColumnFiltersChange: setColumnFilters,
    onColumnVisibilityChange: setColumnVisibility,
    getCoreRowModel: getCoreRowModel(),
    getRowId: (row) => row.id,
  });

  return (
    <>
      <SiteHeader title="Event Logs" />
      <div className="flex flex-1 flex-col gap-4 p-4 md:p-6">
        {error && <p className="text-destructive text-sm">{error}</p>}

        <DataTable
          table={table}
          showPagination={false}
          onRowClick={setViewEvent}
        >
          <DataTableToolbar table={table} />
        </DataTable>

        <div className="flex w-full items-center justify-between gap-4 overflow-auto p-1">
          <p className="text-muted-foreground flex-1 whitespace-nowrap text-sm">
            {events.length} event(s) on this page.
          </p>
          <div className="flex items-center space-x-2">
            <Button
              aria-label="Go to previous page"
              title="Previous page"
              variant="outline"
              size="icon"
              className="size-8"
              disabled={cursorStack.length === 0 || loading}
              onClick={handlePrevious}
            >
              <ChevronLeft />
            </Button>
            <Button
              aria-label="Go to next page"
              title="Next page"
              variant="outline"
              size="icon"
              className="size-8"
              disabled={!nextCursor || loading}
              onClick={handleNext}
            >
              <ChevronRight />
            </Button>
          </div>
        </div>

        {viewEvent && (
          <ResponsiveDialog open onOpenChange={() => setViewEvent(null)}>
            <ResponsiveDialogContent className="sm:max-w-4xl">
              <ResponsiveDialogHeader>
                <ResponsiveDialogTitle className="flex items-center gap-2">
                  {severityBadge(viewEvent.severity)}
                  <span className="font-mono text-sm">
                    {viewEvent.event_type}
                  </span>
                </ResponsiveDialogTitle>
              </ResponsiveDialogHeader>
              <EventDetailBody event={viewEvent} />
            </ResponsiveDialogContent>
          </ResponsiveDialog>
        )}
      </div>
    </>
  );
}
