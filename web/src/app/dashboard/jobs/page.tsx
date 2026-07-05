"use client";

import { useCallback, useEffect, useState } from "react";
import { toast } from "sonner";
import {
  CheckCircle2,
  ChevronDown,
  Circle,
  Loader2,
  PauseCircle,
  XCircle,
} from "lucide-react";

import { useCurrentUser } from "@/hooks/use-current-user";
import { toastError } from "@/lib/format";
import {
  cancelJob,
  getJobs,
  pauseJob,
  resumeJob,
} from "@/lib/api";
import type { Job } from "@/types/jobs";
import { SiteHeader } from "@/components/site-header";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@/components/ui/collapsible";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  Sheet,
  SheetContent,
  SheetFooter,
  SheetHeader,
  SheetTitle,
} from "@/components/ui/sheet";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";

const STATUS_OPTIONS = [
  { value: "all", label: "All statuses" },
  { value: "pending", label: "Pending" },
  { value: "running", label: "Running" },
  { value: "paused", label: "Paused" },
  { value: "completed", label: "Completed" },
  { value: "failed", label: "Failed" },
  { value: "cancelled", label: "Cancelled" },
] as const;

function statusBadge(status: string) {
  switch (status) {
    case "completed":
      return (
        <Badge className="bg-emerald-500/15 text-emerald-700 dark:text-emerald-400 hover:bg-emerald-500/25 border-emerald-500/20">
          completed
        </Badge>
      );
    case "failed":
      return <Badge variant="destructive">failed</Badge>;
    case "cancelled":
      return (
        <Badge className="bg-amber-500/15 text-amber-700 dark:text-amber-400 hover:bg-amber-500/25 border-amber-500/20">
          cancelled
        </Badge>
      );
    case "cancelling":
      return (
        <Badge className="bg-amber-500/15 text-amber-700 dark:text-amber-400 hover:bg-amber-500/25 border-amber-500/20">
          cancelling
        </Badge>
      );
    case "pausing":
      return (
        <Badge className="bg-gray-500/15 text-gray-700 dark:text-gray-400 hover:bg-gray-500/25 border-gray-500/20">
          pausing
        </Badge>
      );
    case "paused":
      return (
        <Badge className="bg-gray-500/15 text-gray-700 dark:text-gray-400 hover:bg-gray-500/25 border-gray-500/20">
          paused
        </Badge>
      );
    case "running":
      return (
        <Badge className="bg-blue-500/15 text-blue-700 dark:text-blue-400 hover:bg-blue-500/25 border-blue-500/20">
          running
        </Badge>
      );
    case "pending":
      return <Badge variant="secondary">pending</Badge>;
    default:
      return <Badge variant="secondary">{status}</Badge>;
  }
}

function statusIcon(status: string) {
  switch (status) {
    case "completed":
      return <CheckCircle2 className="size-4 text-emerald-500" />;
    case "failed":
      return <XCircle className="size-4 text-destructive" />;
    case "cancelled":
      return <XCircle className="size-4 text-amber-500" />;
    case "cancelling":
      return <Loader2 className="size-4 animate-spin text-amber-500" />;
    case "pausing":
      return <Loader2 className="size-4 animate-spin text-gray-500" />;
    case "paused":
      return <PauseCircle className="size-4 text-gray-500" />;
    case "running":
      return <Loader2 className="size-4 animate-spin text-blue-500" />;
    default:
      return <Circle className="size-4 text-muted-foreground" />;
  }
}

function hasContent(value: unknown): boolean {
  if (value == null) return false;
  if (typeof value === "object") {
    return Array.isArray(value)
      ? value.length > 0
      : Object.keys(value as Record<string, unknown>).length > 0;
  }
  return true;
}

function relativeTime(dateStr: string): string {
  const now = Date.now();
  const then = new Date(dateStr).getTime();
  const diff = now - then;
  const seconds = Math.floor(diff / 1000);
  if (seconds < 60) return "just now";
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.floor(hours / 24);
  return `${days}d ago`;
}

export default function JobsPage() {
  const { hasPermission } = useCurrentUser();
  const [jobs, setJobs] = useState<Job[]>([]);
  const [statusFilter, setStatusFilter] = useState("all");
  const [selectedJobId, setSelectedJobId] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  const load = useCallback(() => {
    const params = statusFilter !== "all" ? { status: statusFilter } : {};
    getJobs(params)
      .then((resp) => {
        setJobs(resp.jobs);
        setLoading(false);
      })
      .catch((e) => {
        toastError("Failed to load jobs", e);
        setLoading(false);
      });
  }, [statusFilter]);

  useEffect(() => {
    setLoading(true);
    load();
  }, [load]);

  // Poll every 5 seconds for active jobs
  const hasActiveJobs = jobs.some(
    (j) =>
      j.status === "running" ||
      j.status === "pending" ||
      j.status === "cancelling" ||
      j.status === "pausing",
  );

  useEffect(() => {
    const interval = setInterval(load, hasActiveJobs ? 3000 : 10000);
    return () => clearInterval(interval);
  }, [load, hasActiveJobs]);

  const selectedJob = jobs.find((j) => j.id === selectedJobId) ?? null;
  const canManage = hasPermission("jobs:manage");

  return (
    <>
      <SiteHeader title="Jobs" />
      <div className="flex flex-1 flex-col gap-4 p-4 md:p-6">
        <div className="flex items-center justify-between">
          <h2 className="text-lg font-semibold">Background Jobs</h2>
          <Select value={statusFilter} onValueChange={setStatusFilter}>
            <SelectTrigger className="w-[160px]">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {STATUS_OPTIONS.map((opt) => (
                <SelectItem key={opt.value} value={opt.value}>
                  {opt.label}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>

        <div className="rounded-lg border">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead className="w-[40px]" />
                <TableHead>Type</TableHead>
                <TableHead>Status</TableHead>
                <TableHead>Created by</TableHead>
                <TableHead>Created</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {loading && jobs.length === 0 && (
                <TableRow>
                  <TableCell
                    colSpan={5}
                    className="text-muted-foreground text-center py-8"
                  >
                    <Loader2 className="size-4 animate-spin mx-auto" />
                  </TableCell>
                </TableRow>
              )}
              {!loading && jobs.length === 0 && (
                <TableRow>
                  <TableCell
                    colSpan={5}
                    className="text-muted-foreground text-center py-8"
                  >
                    {statusFilter !== "all"
                      ? `No ${statusFilter} jobs.`
                      : "No background jobs yet. Jobs are created by Lua scripts via jobs.create()."}
                  </TableCell>
                </TableRow>
              )}
              {jobs.map((job) => (
                <TableRow
                  key={job.id}
                  className="cursor-pointer focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                  tabIndex={0}
                  role="button"
                  onClick={() => setSelectedJobId(job.id)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" || e.key === " ") {
                      e.preventDefault();
                      setSelectedJobId(job.id);
                    }
                  }}
                >
                  <TableCell className="pr-0">
                    {statusIcon(job.status)}
                  </TableCell>
                  <TableCell className="font-mono text-sm">
                    {job.job_type}
                  </TableCell>
                  <TableCell>{statusBadge(job.status)}</TableCell>
                  <TableCell
                    className="font-mono text-xs max-w-[200px] truncate"
                    title={job.created_by}
                  >
                    {job.created_by}
                  </TableCell>
                  <TableCell
                    className="text-muted-foreground text-sm"
                    title={new Date(job.created_at).toLocaleString()}
                  >
                    {relativeTime(job.created_at)}
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>

        <Sheet
          open={selectedJob != null}
          onOpenChange={(open) => {
            if (!open) {
              setSelectedJobId(null);
              load();
            }
          }}
        >
          <SheetContent className="sm:max-w-xl overflow-hidden flex flex-col">
            {selectedJob && (
              <JobDetail
                job={selectedJob}
                canManage={canManage}
                onAction={load}
              />
            )}
          </SheetContent>
        </Sheet>
      </div>
    </>
  );
}

function JobDetail({
  job,
  canManage,
  onAction,
}: {
  job: Job;
  canManage: boolean;
  onAction: () => void;
}) {
  const [actionLoading, setActionLoading] = useState<string | null>(null);
  const isActive =
    job.status === "running" ||
    job.status === "cancelling" ||
    job.status === "pausing";

  async function handleCancel() {
    setActionLoading("cancel");
    try {
      await cancelJob(job.id);
      toast.success("Job cancelled");
      onAction();
    } catch (e) {
      toastError("Failed to cancel job", e);
    } finally {
      setActionLoading(null);
    }
  }

  async function handlePause() {
    setActionLoading("pause");
    try {
      await pauseJob(job.id);
      toast.success("Job paused");
      onAction();
    } catch (e) {
      toastError("Failed to pause job", e);
    } finally {
      setActionLoading(null);
    }
  }

  async function handleResume() {
    setActionLoading("resume");
    try {
      await resumeJob(job.id);
      toast.success("Job resumed");
      onAction();
    } catch (e) {
      toastError("Failed to resume job", e);
    } finally {
      setActionLoading(null);
    }
  }

  return (
    <>
      <SheetHeader>
        <SheetTitle className="flex items-center gap-2">
          <span className="font-mono text-sm">Job Details</span>
        </SheetTitle>
      </SheetHeader>
      <div className="flex-1 min-h-0 overflow-y-auto px-4 flex flex-col gap-4">
        <div className="grid grid-cols-2 gap-4 text-sm">
          <div className="col-span-2">
            <span className="text-muted-foreground">Job ID</span>
            <p className="font-mono text-xs break-all">{job.id}</p>
          </div>
          <div>
            <span className="text-muted-foreground">Type</span>
            <p className="font-mono text-xs">{job.job_type}</p>
          </div>
          <div>
            <span className="text-muted-foreground">Status</span>
            <div className="mt-0.5">{statusBadge(job.status)}</div>
          </div>
          <div>
            <span className="text-muted-foreground">Created by</span>
            <p className="font-mono text-xs break-all">{job.created_by}</p>
          </div>
          <div>
            <span className="text-muted-foreground">Created</span>
            <p className="text-xs">
              {new Date(job.created_at).toLocaleString()}
            </p>
          </div>
          {job.started_at && (
            <div>
              <span className="text-muted-foreground">Started</span>
              <p className="text-xs">
                {new Date(job.started_at).toLocaleString()}
              </p>
            </div>
          )}
          {job.completed_at && (
            <div>
              <span className="text-muted-foreground">Completed</span>
              <p className="text-xs">
                {new Date(job.completed_at).toLocaleString()}
              </p>
            </div>
          )}
        </div>

        {job.error && (
          <div>
            <span className="text-muted-foreground text-sm">Error</span>
            <div className="bg-destructive/10 text-destructive mt-1 rounded-md p-3 font-mono text-xs whitespace-pre-wrap">
              {job.error}
            </div>
          </div>
        )}

        <JsonSection title="Input" data={job.input} defaultOpen />
        <JsonSection title="Progress" data={job.progress} defaultOpen={isActive} />
        {hasContent(job.result) && <JsonSection title="Result" data={job.result} />}
      </div>

      {canManage && (
        <SheetFooter className="border-t flex-row justify-end gap-2">
          {(job.status === "running" || job.status === "pausing") && (
            <Button
              variant="outline"
              size="sm"
              disabled={actionLoading !== null || job.status === "pausing"}
              onClick={handlePause}
            >
              {actionLoading === "pause" || job.status === "pausing"
                ? "Pausing…"
                : "Pause Job"}
            </Button>
          )}
          {job.status === "paused" && (
            <Button
              variant="default"
              size="sm"
              disabled={actionLoading !== null}
              onClick={handleResume}
            >
              {actionLoading === "resume" ? "Resuming…" : "Resume Job"}
            </Button>
          )}
          {isActive && (
            <Button
              variant="destructive"
              size="sm"
              disabled={
                actionLoading !== null || job.status === "cancelling"
              }
              onClick={handleCancel}
            >
              {job.status === "cancelling"
                ? "Cancelling…"
                : "Cancel Job"}
            </Button>
          )}
          {job.status === "paused" && (
            <Button
              variant="destructive"
              size="sm"
              disabled={actionLoading !== null}
              onClick={handleCancel}
            >
              Cancel Job
            </Button>
          )}
        </SheetFooter>
      )}
    </>
  );
}

function JsonSection({
  title,
  data,
  defaultOpen = false,
}: {
  title: string;
  data: unknown;
  defaultOpen?: boolean;
}) {
  const [open, setOpen] = useState(defaultOpen);
  const empty = !hasContent(data);

  if (empty) return null;

  return (
    <Collapsible open={open} onOpenChange={setOpen}>
      <CollapsibleTrigger asChild>
        <button
          type="button"
          className="flex w-full items-center gap-1.5 text-sm text-muted-foreground hover:text-foreground transition-colors"
        >
          <ChevronDown
            className={`size-3.5 transition-transform ${open ? "" : "-rotate-90"}`}
          />
          <span>{title}</span>
        </button>
      </CollapsibleTrigger>
      <CollapsibleContent>
        <pre className="mt-1 rounded-md border bg-muted/30 p-3 font-mono text-xs overflow-x-auto max-h-64 overflow-y-auto">
          {JSON.stringify(data, null, 2)}
        </pre>
      </CollapsibleContent>
    </Collapsible>
  );
}
