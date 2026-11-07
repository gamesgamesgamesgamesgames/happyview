"use client";

import { type ReactNode, useMemo, useState } from "react";
import { ChevronDown, ChevronUp, ChevronsUpDown, Loader2 } from "lucide-react";

import { formatBytes } from "@/lib/format";
import { cn } from "@/lib/utils";
import type { ScriptMetricsRow } from "@/types/metrics";
import { Badge } from "@/components/ui/badge";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";

import {
  errorCount,
  failureBreakdown,
  formatMs,
  kindLabel,
  timeoutCount,
} from "./metrics-format";

interface Column {
  id: string;
  label: string;
  /** What the column sorts by; a missing number sorts below every real one. */
  sortValue: (row: ScriptMetricsRow) => number | string | null;
  cell: (row: ScriptMetricsRow) => ReactNode;
  numeric?: boolean;
  /** What a terse header means, on hover. */
  hint?: string;
}

function failureCell(n: number, title?: string) {
  return (
    <span
      className={
        n > 0 ? "text-destructive font-medium" : "text-muted-foreground"
      }
      title={title}
    >
      {n.toLocaleString()}
    </span>
  );
}

function number(
  id: string,
  label: string,
  value: (row: ScriptMetricsRow) => number | null,
  cell: (row: ScriptMetricsRow) => ReactNode,
  hint?: string,
): Column {
  return { id, label, sortValue: value, cell, numeric: true, hint };
}

const COLUMNS: Column[] = [
  {
    id: "trigger",
    label: "Script",
    sortValue: (r) => r.trigger,
    cell: (r) => (
      <span
        className="block max-w-40 truncate font-mono text-xs"
        title={r.trigger}
      >
        {r.trigger}
      </span>
    ),
  },
  {
    id: "kind",
    label: "Kind",
    sortValue: (r) => kindLabel(r.kind),
    cell: (r) => <Badge variant="outline">{kindLabel(r.kind)}</Badge>,
  },
  {
    id: "layer",
    label: "Source",
    sortValue: (r) => r.layer,
    cell: (r) =>
      r.layer === "project" ? (
        <Badge variant="secondary">Project file</Badge>
      ) : (
        <span className="text-sm text-muted-foreground">Dashboard</span>
      ),
  },
  number(
    "runs",
    "Runs",
    (r) => r.runs,
    (r) => r.runs.toLocaleString(),
  ),
  number(
    "p50",
    "p50",
    (r) => r.wall_ms.p50,
    (r) => formatMs(r.wall_ms.p50),
  ),
  number(
    "p95",
    "p95",
    (r) => r.wall_ms.p95,
    (r) => formatMs(r.wall_ms.p95),
  ),
  number(
    "max",
    "Max",
    (r) => r.wall_ms.max,
    (r) => formatMs(r.wall_ms.max),
  ),
  number(
    "errors",
    "Errors",
    (r) => errorCount(r.outcomes),
    (r) =>
      failureCell(
        errorCount(r.outcomes),
        failureBreakdown(r.outcomes) || undefined,
      ),
  ),
  number(
    "timeouts",
    "Timeouts",
    (r) => timeoutCount(r.outcomes),
    (r) => failureCell(timeoutCount(r.outcomes)),
  ),
  number(
    "memory",
    "Memory peak",
    (r) => r.memory_peak_bytes.max,
    (r) =>
      r.memory_peak_bytes.max == null
        ? "—"
        : formatBytes(r.memory_peak_bytes.max),
    "The most memory any run reached",
  ),
  number(
    "guest",
    "Code · host",
    (r) => r.guest_ms.mean,
    (r) => (
      <span className="whitespace-nowrap">
        {formatMs(r.guest_ms.mean)}
        <span className="text-muted-foreground"> · </span>
        {formatMs(r.host_ms_mean)}
      </span>
    ),
    "Mean time in the script's own code, then waiting on the host calls it made. Sorts by code.",
  ),
];

interface Sort {
  id: string;
  desc: boolean;
}

function compare(a: number | string | null, b: number | string | null): number {
  if (a == null && b == null) return 0;
  if (a == null) return -1;
  if (b == null) return 1;
  if (typeof a === "string" || typeof b === "string") {
    return String(a).localeCompare(String(b));
  }
  return a - b;
}

/** Every script that ran in the window, busiest first by default. */
export function ScriptMetricsTable({
  rows,
  loading,
  emptyText,
  onSelect,
}: {
  rows: ScriptMetricsRow[];
  loading: boolean;
  emptyText: string;
  onSelect: (trigger: string) => void;
}) {
  const [sort, setSort] = useState<Sort>({ id: "runs", desc: true });

  const sorted = useMemo(() => {
    const column = COLUMNS.find((c) => c.id === sort.id) ?? COLUMNS[3];
    const sign = sort.desc ? -1 : 1;
    return [...rows].sort(
      (a, b) => sign * compare(column.sortValue(a), column.sortValue(b)),
    );
  }, [rows, sort]);

  function toggle(column: Column) {
    setSort((current) =>
      current.id === column.id
        ? { id: column.id, desc: !current.desc }
        : // Numbers start busiest or slowest first, names from A.
          { id: column.id, desc: column.numeric === true },
    );
  }

  return (
    <div className="min-w-0 rounded-lg border">
      <Table>
        <TableHeader>
          <TableRow>
            {COLUMNS.map((column) => {
              const active = sort.id === column.id;
              const Icon = !active
                ? ChevronsUpDown
                : sort.desc
                  ? ChevronDown
                  : ChevronUp;
              return (
                <TableHead
                  key={column.id}
                  className={column.numeric ? "text-right" : undefined}
                  aria-sort={
                    active
                      ? sort.desc
                        ? "descending"
                        : "ascending"
                      : undefined
                  }
                >
                  <button
                    type="button"
                    title={column.hint}
                    onClick={() => toggle(column)}
                    className={cn(
                      "-mx-2 inline-flex h-8 items-center gap-1.5 rounded-md px-2 hover:bg-accent focus:outline-none focus-visible:ring-1 focus-visible:ring-ring [&_svg]:size-4 [&_svg]:shrink-0 [&_svg]:text-muted-foreground",
                      column.numeric && "flex-row-reverse",
                    )}
                  >
                    {column.label}
                    <Icon aria-hidden />
                  </button>
                </TableHead>
              );
            })}
          </TableRow>
        </TableHeader>
        <TableBody>
          {loading && sorted.length === 0 && (
            <TableRow>
              <TableCell colSpan={COLUMNS.length} className="py-8">
                <Loader2 className="size-4 animate-spin mx-auto text-muted-foreground" />
              </TableCell>
            </TableRow>
          )}
          {!loading && sorted.length === 0 && (
            <TableRow>
              <TableCell
                colSpan={COLUMNS.length}
                className="text-muted-foreground text-center py-8"
              >
                {emptyText}
              </TableCell>
            </TableRow>
          )}
          {sorted.map((row) => (
            <TableRow
              key={`${row.trigger} ${row.kind} ${row.layer}`}
              className="cursor-pointer focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
              tabIndex={0}
              onClick={() => onSelect(row.trigger)}
              onKeyDown={(e) => {
                if (e.key === "Enter" || e.key === " ") {
                  e.preventDefault();
                  onSelect(row.trigger);
                }
              }}
            >
              {COLUMNS.map((column) => (
                <TableCell
                  key={column.id}
                  className={
                    column.numeric ? "text-right tabular-nums" : undefined
                  }
                >
                  {column.cell(row)}
                </TableCell>
              ))}
            </TableRow>
          ))}
        </TableBody>
      </Table>
    </div>
  );
}
