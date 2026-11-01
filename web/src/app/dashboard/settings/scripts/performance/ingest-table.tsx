import { Loader2 } from "lucide-react";

import type { IngestRow } from "@/types/metrics";
import { Badge } from "@/components/ui/badge";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";

import { formatMs, rateText, shareText, stageMeans } from "./metrics-format";
import { ShareBar } from "./time-breakdown";

/** The record pipeline: one row per collection and action. */
export function IngestTable({
  rows,
  loading,
  emptyText,
}: {
  rows: IngestRow[];
  loading: boolean;
  emptyText: string;
}) {
  return (
    <div className="min-w-0 rounded-lg border">
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>Collection</TableHead>
            <TableHead className="text-right">Records</TableHead>
            <TableHead className="text-right">Rate</TableHead>
            <TableHead className="text-right">p50</TableHead>
            <TableHead className="text-right">p95</TableHead>
            <TableHead className="min-w-40">Script vs platform</TableHead>
            <TableHead className="min-w-48">Stage means</TableHead>
            <TableHead className="text-right">Failed</TableHead>
            <TableHead className="text-right">Timeouts</TableHead>
            <TableHead className="text-right">Dead-lettered</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {loading && rows.length === 0 && (
            <TableRow>
              <TableCell colSpan={10} className="py-8">
                <Loader2 className="size-4 animate-spin mx-auto text-muted-foreground" />
              </TableCell>
            </TableRow>
          )}
          {!loading && rows.length === 0 && (
            <TableRow>
              <TableCell
                colSpan={10}
                className="text-muted-foreground text-center py-8"
              >
                {emptyText}
              </TableCell>
            </TableRow>
          )}
          {rows.map((row) => (
            <TableRow key={`${row.collection} ${row.action}`}>
              <TableCell>
                <div className="flex items-center gap-2">
                  <span
                    className="block max-w-40 truncate font-mono text-xs"
                    title={row.collection}
                  >
                    {row.collection}
                  </span>
                  <Badge variant="outline">{row.action}</Badge>
                </div>
              </TableCell>
              <TableCell className="text-right tabular-nums">
                {row.records.toLocaleString()}
              </TableCell>
              <TableCell className="text-right tabular-nums">
                {rateText(row.records_per_s)}
              </TableCell>
              <TableCell className="text-right tabular-nums">
                {formatMs(row.record_ms.p50)}
              </TableCell>
              <TableCell className="text-right tabular-nums">
                {formatMs(row.record_ms.p95)}
              </TableCell>
              <TableCell>
                {row.script_share == null ? (
                  <span className="text-xs text-muted-foreground">No hook</span>
                ) : (
                  <div className="flex flex-col gap-1">
                    <ShareBar scriptShare={row.script_share} />
                    <span className="text-xs text-muted-foreground tabular-nums">
                      script {shareText(row.script_share)} · platform{" "}
                      {shareText(row.platform_share ?? 1 - row.script_share)}
                    </span>
                  </div>
                )}
              </TableCell>
              <TableCell>
                <StageMeans row={row} />
              </TableCell>
              <Count n={row.failed} />
              <Count n={row.timeouts} />
              <Count n={row.dead_lettered} />
            </TableRow>
          ))}
        </TableBody>
      </Table>
    </div>
  );
}

function StageMeans({ row }: { row: IngestRow }) {
  const means = stageMeans(row.stages);
  if (means.length === 0) {
    return <span className="text-xs text-muted-foreground">—</span>;
  }
  return (
    <ul className="flex flex-wrap gap-x-3 gap-y-0.5 text-xs text-muted-foreground tabular-nums">
      {means.map((m) => (
        <li
          key={m.name}
          className={m.partOfHook ? "opacity-70" : undefined}
          title={m.partOfHook ? "Part of the hook stage" : undefined}
        >
          {m.partOfHook && <span aria-hidden>↳ </span>}
          {m.label} {formatMs(m.ms)}
        </li>
      ))}
      {row.script_ms_mean != null && (
        <li className="text-foreground">
          script {formatMs(row.script_ms_mean)}
        </li>
      )}
    </ul>
  );
}

function Count({ n }: { n: number }) {
  return (
    <TableCell
      className={`text-right tabular-nums ${n > 0 ? "text-destructive font-medium" : "text-muted-foreground"}`}
    >
      {n.toLocaleString()}
    </TableCell>
  );
}
