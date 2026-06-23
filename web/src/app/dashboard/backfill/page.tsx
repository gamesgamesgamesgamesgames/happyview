"use client";

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { toast } from "sonner";

import { useCurrentUser } from "@/hooks/use-current-user";
import { toastError } from "@/lib/format";
import {
  cancelBackfillJob,
  pauseBackfillJob,
  resumeBackfillJob,
  createBackfillJob,
  getBackfillJobs,
  getBackfillRepos,
  getBackfillPdsSummary,
  flushBackfillDetails,
  flushAllBackfillDetails,
  getLexicons,
} from "@/lib/api";
import type {
  BackfillJob,
  BackfillRepoEntry,
  PdsSummaryEntry,
  BackfillEvent,
  BlueskyProfile,
} from "@/types/backfill";
import { CheckCircle2, ChevronRight, Circle, Loader2, PauseCircle } from "lucide-react";
import { SiteHeader } from "@/components/site-header";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from "@/components/ui/alert-dialog";
import { Badge } from "@/components/ui/badge";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@/components/ui/collapsible";
import { Button } from "@/components/ui/button";
import {
  Combobox,
  ComboboxContent,
  ComboboxEmpty,
  ComboboxInput,
  ComboboxItem,
  ComboboxList,
} from "@/components/ui/combobox";
import {
  ResponsiveDialog,
  ResponsiveDialogClose,
  ResponsiveDialogContent,
  ResponsiveDialogDescription,
  ResponsiveDialogFooter,
  ResponsiveDialogHeader,
  ResponsiveDialogTitle,
  ResponsiveDialogTrigger,
} from "@/components/ui/responsive-dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
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

const PROGRESS_PHASES = [
  "discovering_repos",
  "resolving_pds",
  "fetching_records",
] as const;

function statusBadge(job: BackfillJob) {
  switch (job.status) {
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
          {job.stage === "pending" ? "starting" : job.stage.replace(/_/g, " ")}
        </Badge>
      );
    default:
      return <Badge variant="secondary">{job.status}</Badge>;
  }
}

function phaseIndex(stage: string): number {
  if (stage === "resolving_and_fetching") return 1;
  const idx = PROGRESS_PHASES.indexOf(
    stage as (typeof PROGRESS_PHASES)[number],
  );
  return idx;
}

// SSE via Web Worker — events are batched off the main thread and flushed periodically
function useBackfillSSE(
  jobId: string | null,
  active: boolean,
  onBatch: (events: BackfillEvent[]) => void,
) {
  const onBatchRef = useRef(onBatch);
  onBatchRef.current = onBatch;

  useEffect(() => {
    if (!jobId || !active) return;

    const worker = new Worker(
      new URL("@/workers/backfill-sse.worker.ts", import.meta.url),
    );

    worker.addEventListener("message", (e: MessageEvent) => {
      if (e.data.type === "batch") {
        onBatchRef.current(e.data.events as BackfillEvent[]);
      }
    });

    const basePath = process.env.NEXT_PUBLIC_BASE_PATH || "";
    worker.postMessage({ type: "connect", jobId, baseUrl: `${location.origin}${basePath}` });

    return () => {
      worker.postMessage({ type: "disconnect" });
      worker.terminate();
    };
  }, [jobId, active]);
}

// Batch Bluesky profile resolution hook
function useBlueskyProfiles(dids: string[]): Map<string, BlueskyProfile> {
  const [profiles, setProfiles] = useState<Map<string, BlueskyProfile>>(new Map());
  const resolvedRef = useRef<Set<string>>(new Set());
  const pendingRef = useRef(false);

  useEffect(() => {
    const unresolved = dids.filter((d) => !resolvedRef.current.has(d));
    if (unresolved.length === 0 || pendingRef.current) return;

    pendingRef.current = true;

    const batches: string[][] = [];
    for (let i = 0; i < unresolved.length; i += 25) {
      batches.push(unresolved.slice(i, i + 25));
    }

    // Mark all as resolved immediately to prevent re-fetching
    for (const did of unresolved) {
      resolvedRef.current.add(did);
    }

    Promise.all(
      batches.map(async (batch) => {
        const params = batch.map((d) => `actors=${encodeURIComponent(d)}`).join("&");
        try {
          const resp = await fetch(
            `https://public.api.bsky.app/xrpc/app.bsky.actor.getProfiles?${params}`
          );
          if (!resp.ok) return [];
          const data = await resp.json();
          return (data.profiles || []) as BlueskyProfile[];
        } catch {
          return [];
        }
      })
    ).then((results) => {
      setProfiles((prev) => {
        const newProfiles = new Map(prev);
        for (const batch of results) {
          for (const p of batch) {
            newProfiles.set(p.did, p);
          }
        }
        return newProfiles;
      });
      pendingRef.current = false;
    });
  }, [dids]);

  return profiles;
}

const BSKY_PDS_SUFFIX = ".bsky.network";
const BSKY_PDS_HOSTNAMES = ["bsky.social", "staging.bsky.dev"];
const failedFaviconUrls = new Set<string>();

function isBskyPds(pdsEndpoint: string): boolean {
  try {
    const hostname = new URL(pdsEndpoint).hostname;
    return BSKY_PDS_HOSTNAMES.includes(hostname) || hostname.endsWith(BSKY_PDS_SUFFIX);
  } catch {
    return false;
  }
}

function PdsFavicon({ pdsEndpoint }: { pdsEndpoint: string }) {
  let hostname: string;
  try {
    hostname = new URL(pdsEndpoint).hostname;
  } catch {
    return <PdsPlaceholderIcon />;
  }

  if (isBskyPds(pdsEndpoint)) {
    return (
      <svg viewBox="0 0 360 320" className="size-4 shrink-0" aria-hidden>
        <path
          d="M180 142c-16.3-31.7-60.7-90.8-102-120C38.5-5.9 0 1.4 0 45.6c0 31.7 18.2 266.4 67.8 266.4 45.2 0 60.4-39 112.2-130 51.8 91 67 130 112.2 130C341.8 312 360 77.3 360 45.6 360 1.4 321.5-5.9 282 22c-41.3 29.2-85.7 88.3-102 120Z"
          fill="#1185FE"
        />
      </svg>
    );
  }

  const faviconUrl = `https://twenty-icons.com/${hostname}`;

  if (failedFaviconUrls.has(faviconUrl)) {
    return <PdsPlaceholderIcon />;
  }

  return <PdsFaviconImg url={faviconUrl} />;
}

function PdsFaviconImg({ url }: { url: string }) {
  const [failed, setFailed] = useState(false);

  if (failed) return <PdsPlaceholderIcon />;

  return (
    <img
      src={url}
      alt=""
      className="size-4 shrink-0 rounded"
      onError={() => { failedFaviconUrls.add(url); setFailed(true); }}
    />
  );
}

function PdsPlaceholderIcon() {
  return (
    <svg viewBox="0 0 16 16" className="size-4 shrink-0 text-muted-foreground" fill="none" stroke="currentColor" strokeWidth="1.2" aria-hidden>
      <ellipse cx="8" cy="12" rx="6" ry="2.5" />
      <ellipse cx="8" cy="8" rx="6" ry="2.5" />
      <path d="M2 8v4M14 8v4" />
      <ellipse cx="8" cy="4" rx="6" ry="2.5" />
      <path d="M2 4v4M14 4v4" />
    </svg>
  );
}

function AnimatedNumber({ value }: { value: number }) {
  const targetRef = useRef(value);
  const displayedRef = useRef(value);
  const [displayed, setDisplayed] = useState(value);
  const rafRef = useRef<number>(0);

  targetRef.current = value;

  useEffect(() => {
    cancelAnimationFrame(rafRef.current);

    function tick() {
      const current = displayedRef.current;
      const target = targetRef.current;
      const diff = target - current;

      if (Math.abs(diff) < 0.5) {
        displayedRef.current = target;
        setDisplayed(target);
        return;
      }

      const next = current + diff * 0.06;
      displayedRef.current = next;
      setDisplayed(next);
      rafRef.current = requestAnimationFrame(tick);
    }

    tick();
    return () => cancelAnimationFrame(rafRef.current);
  }, [value]);

  return <>{Math.round(displayed).toLocaleString()}</>;
}

function CompactRepoRow({ did, profile }: {
  did: string;
  profile?: BlueskyProfile;
}) {
  return (
    <div className="flex items-center gap-1.5 px-3 py-1 text-xs">
      <div className="size-4 shrink-0 rounded-full bg-muted overflow-hidden">
        {profile?.avatar && (
          <img src={profile.avatar} alt="" className="size-full object-cover" />
        )}
      </div>
      <span className="truncate">
        {profile?.handle ? `@${profile.handle}` : <span className="font-mono">{did}</span>}
      </span>
    </div>
  );
}

function ProfileRow({ did, profile, suffix }: {
  did: string;
  profile?: BlueskyProfile;
  suffix?: React.ReactNode;
}) {
  return (
    <div className="flex items-center gap-2 px-3 py-1.5 text-sm">
      <div className="size-6 shrink-0 rounded-full bg-muted overflow-hidden">
        {profile?.avatar && (
          <img src={profile.avatar} alt="" className="size-full object-cover" />
        )}
      </div>
      <div className="flex-1 min-w-0">
        <p className="truncate text-sm">
          {profile?.displayName || profile?.handle || did}
        </p>
        {profile?.handle && (
          <p className="truncate text-xs text-muted-foreground">@{profile.handle}</p>
        )}
        {!profile?.handle && (
          <p className="truncate text-xs text-muted-foreground font-mono">{did}</p>
        )}
      </div>
      {suffix && (
        <span className="shrink-0 text-xs text-muted-foreground tabular-nums">{suffix}</span>
      )}
    </div>
  );
}

export default function BackfillPage() {
  const { hasPermission } = useCurrentUser();
  const [jobs, setJobs] = useState<BackfillJob[]>([]);
  const [selectedJobId, setSelectedJobId] = useState<string | null>(null);

  const load = useCallback(() => {
    getBackfillJobs()
      .then(setJobs)
      .catch((e) => toastError("Failed to load backfill jobs", e));
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const selectedJob = jobs.find((j) => j.id === selectedJobId) ?? null;
  const sseActive = selectedJob != null && (selectedJob.status === "running" || selectedJob.status === "cancelling" || selectedJob.status === "pausing");

  useEffect(() => {
    if (sseActive) return;
    const interval = setInterval(load, 5000);
    return () => clearInterval(interval);
  }, [load, sseActive]);
  const canFlush = hasPermission("backfill:create");

  return (
    <>
      <SiteHeader title="Backfill" />
      <div className="flex flex-1 flex-col gap-4 p-4 md:p-6">
        <div className="flex items-center justify-between">
          <h2 className="text-lg font-semibold">Backfill Jobs</h2>
          <div className="flex items-center gap-2">
            {canFlush && (
              <AlertDialog>
                <AlertDialogTrigger asChild>
                  <Button variant="outline" size="sm">Clear all details</Button>
                </AlertDialogTrigger>
                <AlertDialogContent>
                  <AlertDialogHeader>
                    <AlertDialogTitle>Clear all job details?</AlertDialogTitle>
                    <AlertDialogDescription>
                      This will permanently delete per-repo detail data for all backfill jobs.
                    </AlertDialogDescription>
                  </AlertDialogHeader>
                  <AlertDialogFooter>
                    <AlertDialogCancel>Cancel</AlertDialogCancel>
                    <AlertDialogAction onClick={async () => {
                      await flushAllBackfillDetails();
                      toast.success("All job details cleared");
                      setSelectedJobId(null);
                      load();
                    }}>Clear</AlertDialogAction>
                  </AlertDialogFooter>
                </AlertDialogContent>
              </AlertDialog>
            )}
            {hasPermission("backfill:create") && (
              <CreateDialog onSuccess={load} />
            )}
          </div>
        </div>

        <div className="rounded-lg border">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>ID</TableHead>
                <TableHead>Collection</TableHead>
                <TableHead>DID</TableHead>
                <TableHead>Status</TableHead>
                <TableHead>Started</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {jobs.length === 0 && (
                <TableRow>
                  <TableCell
                    colSpan={5}
                    className="text-muted-foreground text-center"
                  >
                    No backfill jobs yet. Create a job to import historical records from the AT Protocol network.
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
                  <TableCell className="font-mono text-xs">
                    {job.id.slice(0, 8)}
                  </TableCell>
                  <TableCell className="font-mono text-sm">
                    {job.collection ?? "All"}
                  </TableCell>
                  <TableCell className="font-mono text-sm">
                    {job.did ?? "All"}
                  </TableCell>
                  <TableCell>{statusBadge(job)}</TableCell>
                  <TableCell>
                    {job.started_at
                      ? new Date(job.started_at).toLocaleString()
                      : "--"}
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
                canCancel={hasPermission("backfill:create")}
                canFlush={canFlush}
                onJobUpdate={(updater) => {
                  setJobs((prev) =>
                    prev.map((j) =>
                      j.id === selectedJob.id ? updater(j) : j,
                    ),
                  );
                }}
                onCancel={async () => {
                  await cancelBackfillJob(selectedJob.id);
                  load();
                }}
                onPause={async () => {
                  await pauseBackfillJob(selectedJob.id);
                  load();
                }}
                onResume={async () => {
                  await resumeBackfillJob(selectedJob.id);
                  load();
                }}
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
  canCancel,
  canFlush,
  onJobUpdate,
  onCancel,
  onPause,
  onResume,
}: {
  job: BackfillJob;
  canCancel: boolean;
  canFlush: boolean;
  onJobUpdate: (updater: (job: BackfillJob) => BackfillJob) => void;
  onCancel: () => Promise<void>;
  onPause: () => Promise<void>;
  onResume: () => Promise<void>;
}) {
  const [cancelling, setCancelling] = useState(false);
  const [pausing, setPausing] = useState(false);
  const [resuming, setResuming] = useState(false);
  const current = phaseIndex(job.stage);
  const allDone = job.status === "completed";
  const isActive = job.status === "running" || job.status === "cancelling" || job.status === "pausing";
  const isPaused = job.status === "paused" || job.status === "pausing";

  // Detail data state
  const [discoveredRepos, setDiscoveredRepos] = useState<BackfillRepoEntry[]>([]);
  const [discoveredCursor, setDiscoveredCursor] = useState<string | null>(null);
  const [discoveredLoaded, setDiscoveredLoaded] = useState(false);

  const [pdsSummary, setPdsSummary] = useState<PdsSummaryEntry[]>([]);
  const [pdsLoaded, setPdsLoaded] = useState(false);

  const [fetchedRepos, setFetchedRepos] = useState<BackfillRepoEntry[]>([]);
  const [fetchedCursor, setFetchedCursor] = useState<string | null>(null);
  const [fetchedLoaded, setFetchedLoaded] = useState(false);

  // Refs for open state and callbacks so the SSE callback doesn't need to re-bind on toggle
  const onJobUpdateRef = useRef(onJobUpdate);
  onJobUpdateRef.current = onJobUpdate;

  function hasReached(phase: (typeof PROGRESS_PHASES)[number]): boolean {
    if (allDone) return true;
    if (job.stage === "resolving_and_fetching") {
      return phase === "discovering_repos" || phase === "resolving_pds" || phase === "fetching_records";
    }
    return current >= phaseIndex(phase);
  }

  function isPhasePaused(phase: (typeof PROGRESS_PHASES)[number]): boolean {
    if (!isPaused) return false;
    if (job.stage === "resolving_and_fetching") {
      return phase === "resolving_pds" || phase === "fetching_records";
    }
    if (job.stage === "discovering_repos") {
      return phase === "discovering_repos";
    }
    if (job.stage === "resolving_pds") {
      return phase === "resolving_pds";
    }
    if (job.stage === "fetching_records") {
      return phase === "fetching_records";
    }
    return false;
  }

  async function handleCancel() {
    setCancelling(true);
    try {
      await onCancel();
      toast.success("Backfill job cancelled");
    } finally {
      setCancelling(false);
    }
  }

  async function handlePause() {
    setPausing(true);
    try {
      await onPause();
      toast.success("Backfill job paused");
    } finally {
      setPausing(false);
    }
  }

  async function handleResume() {
    setResuming(true);
    try {
      await onResume();
      toast.success("Backfill job resumed");
    } finally {
      setResuming(false);
    }
  }

  const discoveredReached = hasReached("discovering_repos");
  const pdsReached = hasReached("resolving_pds");
  const fetchedReached = hasReached("fetching_records") || job.stage === "resolving_and_fetching";

  // Track which sections are expanded
  const [discoveredOpen, setDiscoveredOpen] = useState(false);
  const [pdsOpen, setPdsOpen] = useState(false);
  const [fetchedOpen, setFetchedOpen] = useState(false);

  // Lazy-load detail data only when sections are expanded
  useEffect(() => {
    if (discoveredOpen && discoveredReached && !discoveredLoaded) {
      getBackfillRepos(job.id, { phase: "discovered", limit: 50 })
        .then((resp) => { setDiscoveredRepos(resp.repos); setDiscoveredCursor(resp.cursor); setDiscoveredLoaded(true); })
        .catch(() => {});
    }
  }, [job.id, discoveredOpen, discoveredReached, discoveredLoaded]);

  useEffect(() => {
    if (pdsOpen && pdsReached && !pdsLoaded) {
      getBackfillPdsSummary(job.id)
        .then((resp) => { setPdsSummary(resp.pds_endpoints); setPdsLoaded(true); })
        .catch(() => {});
    }
  }, [job.id, pdsOpen, pdsReached, pdsLoaded]);

  useEffect(() => {
    if (fetchedOpen && fetchedReached && !fetchedLoaded) {
      getBackfillRepos(job.id, { phase: "fetched", limit: 50 })
        .then((resp) => { setFetchedRepos(resp.repos); setFetchedCursor(resp.cursor); setFetchedLoaded(true); })
        .catch(() => {});
    }
  }, [job.id, fetchedOpen, fetchedReached, fetchedLoaded]);

  const loadMoreDiscovered = useCallback(async () => {
    if (!discoveredCursor) return;
    try {
      const resp = await getBackfillRepos(job.id, { phase: "discovered", cursor: discoveredCursor, limit: 50 });
      setDiscoveredRepos((prev) => [...prev, ...resp.repos]);
      setDiscoveredCursor(resp.cursor);
    } catch { /* ignore */ }
  }, [job.id, discoveredCursor]);

  const loadMoreFetched = useCallback(async () => {
    if (!fetchedCursor) return;
    try {
      const resp = await getBackfillRepos(job.id, { phase: "fetched", cursor: fetchedCursor, limit: 50 });
      setFetchedRepos((prev) => [...prev, ...resp.repos]);
      setFetchedCursor(resp.cursor);
    } catch { /* ignore */ }
  }, [job.id, fetchedCursor]);

  // Process batched SSE events from the Web Worker.
  // Uses refs for open state so the callback identity is stable and doesn't
  // cause the worker to reconnect when sections are toggled.
  const handleSSEBatch = useCallback((events: BackfillEvent[]) => {
    const update = onJobUpdateRef.current;

    for (const e of events) {
      if (e.type === "job_counters") {
        update((j) => ({
          ...j,
          ...(e.total_repos != null && { total_repos: e.total_repos }),
          ...(e.resolved_repos != null && { resolved_repos: e.resolved_repos }),
          ...(e.processed_repos != null && { processed_repos: e.processed_repos }),
          ...(e.total_records != null && { total_records: e.total_records }),
        }));
      } else if (e.type === "job_stage_changed" && e.stage) {
        update((j) => ({ ...j, stage: e.stage! }));
      } else if (e.type === "job_completed" && e.status) {
        update((j) => ({ ...j, status: e.status!, error: e.error ?? null }));
      }
    }
  }, []);

  useBackfillSSE(job.id, isActive, handleSSEBatch);

  // Track visible DIDs from virtualized lists (only items in viewport)
  const [visibleDiscoveredDids, setVisibleDiscoveredDids] = useState<string[]>([]);
  const [visibleFetchedDids, setVisibleFetchedDids] = useState<string[]>([]);

  const allVisibleDids = useMemo(() => {
    const dids = new Set<string>();
    for (const d of visibleDiscoveredDids) dids.add(d);
    for (const d of visibleFetchedDids) dids.add(d);
    return Array.from(dids);
  }, [visibleDiscoveredDids, visibleFetchedDids]);

  const profiles = useBlueskyProfiles(allVisibleDids);

  const sortedPdsSummary = useMemo(
    () => [...pdsSummary].sort((a, b) => b.total_repos - a.total_repos),
    [pdsSummary],
  );

  const fetchedWithRecords = useMemo(
    () => fetchedRepos.filter((r) => r.records_fetched > 0),
    [fetchedRepos],
  );

  return (
    <>
      <SheetHeader>
        <SheetTitle className="flex items-center gap-2">
          <span className="font-mono text-sm">Backfill Details</span>
        </SheetTitle>
      </SheetHeader>
      <div className="flex-1 min-h-0 overflow-y-auto px-4 flex flex-col gap-4">
        <div className="grid grid-cols-2 gap-4 text-sm">
          <div className="col-span-2">
            <span className="text-muted-foreground">Job ID</span>
            <p className="font-mono text-xs break-all">{job.id}</p>
          </div>
          <div>
            <span className="text-muted-foreground">Collection</span>
            <p className="font-mono text-xs">{job.collection ?? "All"}</p>
          </div>
          <div>
            <span className="text-muted-foreground">DID</span>
            <p className="font-mono text-xs break-all">{job.did ?? "All"}</p>
          </div>
          <div>
            <span className="text-muted-foreground">Created</span>
            <p className="text-xs">
              {new Date(job.created_at).toLocaleString()}
            </p>
          </div>
          <div>
            <span className="text-muted-foreground">Started</span>
            <p className="text-xs">
              {job.started_at
                ? new Date(job.started_at).toLocaleString()
                : "--"}
            </p>
          </div>
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

        <div>
          <span className="text-muted-foreground text-sm">Progress</span>
          <div className="mt-1 rounded-md border divide-y">
            <ProgressRow
              label="Discovering repos"
              active={isActive && job.stage === "discovering_repos"}
              reached={hasReached("discovering_repos")}
              paused={isPhasePaused("discovering_repos")}
              value={job.total_repos != null ? <AnimatedNumber value={job.total_repos} /> : undefined}
              suffix="repos found"
              loading={discoveredOpen && discoveredReached && !discoveredLoaded}
              open={discoveredOpen}
              onOpenChange={setDiscoveredOpen}
            >
              {discoveredRepos.length > 0 ? (
                <VirtualList
                  items={discoveredRepos}
                  getKey={(r) => r.did}
                  onVisibleKeysChange={setVisibleDiscoveredDids}
                  hasMore={!!discoveredCursor}
                  onLoadMore={loadMoreDiscovered}
                  rowHeight={28}
                  renderRow={(repo) => (
                    <CompactRepoRow
                      did={repo.did}
                      profile={profiles.get(repo.did)}
                    />
                  )}
                />
              ) : discoveredLoaded ? (
                <p className="py-3 text-center text-xs text-muted-foreground">No repos discovered yet.</p>
              ) : null}
            </ProgressRow>
            <ProgressRow
              label="Resolving PDS"
              active={
                isActive &&
                (job.stage === "resolving_pds" ||
                  job.stage === "resolving_and_fetching")
              }
              reached={hasReached("resolving_pds")}
              paused={isPhasePaused("resolving_pds")}
              value={
                hasReached("resolving_pds")
                  ? <><AnimatedNumber value={job.resolved_repos ?? job.processed_repos ?? 0} /> / <AnimatedNumber value={job.total_repos ?? 0} /></>
                  : undefined
              }
              suffix="resolved"
              loading={pdsOpen && pdsReached && !pdsLoaded}
              open={pdsOpen}
              onOpenChange={setPdsOpen}
            >
              {sortedPdsSummary.length > 0 ? (
                <VirtualList
                  items={sortedPdsSummary}
                  getKey={(p) => p.pds_endpoint}
                  hasMore={false}
                  onLoadMore={() => {}}
                  rowHeight={32}
                  renderRow={(pds) => (
                    <div className="flex items-center gap-2 px-3 py-1.5">
                      <PdsFavicon pdsEndpoint={pds.pds_endpoint} />
                      <span className="flex-1 truncate font-mono text-xs">{new URL(pds.pds_endpoint).hostname}</span>
                      <span className="text-xs text-muted-foreground tabular-nums">
                        <AnimatedNumber value={pds.completed_repos} />/<AnimatedNumber value={pds.total_repos} /> repos · <AnimatedNumber value={pds.total_records} /> records
                      </span>
                    </div>
                  )}
                />
              ) : pdsLoaded ? (
                <p className="py-3 text-center text-xs text-muted-foreground">No PDS data yet.</p>
              ) : null}
            </ProgressRow>
            <ProgressRow
              label="Fetching records"
              active={
                isActive &&
                (job.stage === "fetching_records" ||
                  job.stage === "resolving_and_fetching")
              }
              reached={hasReached("fetching_records")}
              paused={isPhasePaused("fetching_records")}
              value={
                hasReached("fetching_records") ||
                job.stage === "resolving_and_fetching"
                  ? <><AnimatedNumber value={job.processed_repos ?? 0} /> / <AnimatedNumber value={job.total_repos ?? 0} /> repos</>
                  : undefined
              }
              suffix={
                hasReached("fetching_records") ||
                job.stage === "resolving_and_fetching"
                  ? <><AnimatedNumber value={job.total_records ?? 0} /> records</>
                  : undefined
              }
              loading={fetchedOpen && fetchedReached && !fetchedLoaded}
              open={fetchedOpen}
              onOpenChange={setFetchedOpen}
            >
              {fetchedWithRecords.length > 0 ? (
                <VirtualList
                  items={fetchedWithRecords}
                  getKey={(r) => r.did}
                  onVisibleKeysChange={setVisibleFetchedDids}
                  hasMore={!!fetchedCursor}
                  onLoadMore={loadMoreFetched}
                  rowHeight={40}
                  renderRow={(repo) => (
                    <ProfileRow
                      did={repo.did}
                      profile={profiles.get(repo.did)}
                      suffix={<span className="flex items-center gap-1"><CheckCircle2 className="size-3 text-emerald-500" /><AnimatedNumber value={repo.records_fetched} /> records</span>}
                    />
                  )}
                />
              ) : fetchedLoaded ? (
                <p className="py-3 text-center text-xs text-muted-foreground">No repos fetched yet.</p>
              ) : null}
            </ProgressRow>
          </div>
        </div>
      </div>
      <SheetFooter className="border-t flex-row justify-end gap-2">
        {canFlush && !isActive && (
          <AlertDialog>
            <AlertDialogTrigger asChild>
              <Button variant="outline" size="sm">Clear details</Button>
            </AlertDialogTrigger>
            <AlertDialogContent>
              <AlertDialogHeader>
                <AlertDialogTitle>Clear job details?</AlertDialogTitle>
                <AlertDialogDescription>
                  This will permanently delete per-repo detail data for this backfill job.
                </AlertDialogDescription>
              </AlertDialogHeader>
              <AlertDialogFooter>
                <AlertDialogCancel>Cancel</AlertDialogCancel>
                <AlertDialogAction onClick={async () => {
                  await flushBackfillDetails(job.id);
                  toast.success("Job details cleared");
                  setDiscoveredRepos([]);
                  setDiscoveredCursor(null);
                  setDiscoveredLoaded(false);
                  setPdsSummary([]);
                  setPdsLoaded(false);
                  setFetchedRepos([]);
                  setFetchedCursor(null);
                  setFetchedLoaded(false);
                }}>Clear</AlertDialogAction>
              </AlertDialogFooter>
            </AlertDialogContent>
          </AlertDialog>
        )}
        {canCancel && (job.status === "running" || job.status === "pausing") && (
          <Button
            variant="outline"
            size="sm"
            disabled={pausing || job.status === "pausing"}
            onClick={handlePause}
          >
            {pausing || job.status === "pausing" ? "Pausing…" : "Pause Job"}
          </Button>
        )}
        {canCancel && job.status === "paused" && (
          <Button
            variant="default"
            size="sm"
            disabled={resuming}
            onClick={handleResume}
          >
            {resuming ? "Resuming…" : "Resume Job"}
          </Button>
        )}
        {canCancel && isActive && (
          <Button
            variant="destructive"
            size="sm"
            disabled={cancelling || job.status === "cancelling"}
            onClick={handleCancel}
          >
            {job.status === "cancelling" ? "Cancelling…" : "Cancel Job"}
          </Button>
        )}
        {canCancel && job.status === "paused" && (
          <Button
            variant="destructive"
            size="sm"
            disabled={cancelling}
            onClick={handleCancel}
          >
            Cancel Job
          </Button>
        )}
      </SheetFooter>
    </>
  );
}

function VirtualList<T>({
  items,
  getKey,
  hasMore,
  onLoadMore,
  rowHeight,
  renderRow,
  onVisibleKeysChange,
}: {
  items: T[];
  getKey: (item: T) => string;
  hasMore: boolean;
  onLoadMore: () => void;
  rowHeight: number;
  renderRow: (item: T) => React.ReactNode;
  onVisibleKeysChange?: (keys: string[]) => void;
}) {
  const parentRef = useRef<HTMLDivElement>(null);
  const loadMoreTriggered = useRef(false);

  const virtualizer = useVirtualizer({
    count: items.length,
    getScrollElement: () => parentRef.current,
    estimateSize: () => rowHeight,
    overscan: 5,
  });

  const virtualItems = virtualizer.getVirtualItems();

  const visibleKeysStr = virtualItems.map((item) => getKey(items[item.index])).filter(Boolean).join(",");
  useEffect(() => {
    onVisibleKeysChange?.(visibleKeysStr.split(",").filter(Boolean));
  }, [visibleKeysStr, onVisibleKeysChange]);

  useEffect(() => {
    if (!hasMore) return;
    const lastItem = virtualItems[virtualItems.length - 1];
    if (lastItem && lastItem.index >= items.length - 5 && !loadMoreTriggered.current) {
      loadMoreTriggered.current = true;
      onLoadMore();
    }
    if (lastItem && lastItem.index < items.length - 5) {
      loadMoreTriggered.current = false;
    }
  }, [virtualItems, items.length, hasMore, onLoadMore]);

  return (
    <div ref={parentRef} className="max-h-64 overflow-y-auto">
      <div
        style={{ height: virtualizer.getTotalSize(), width: "100%", position: "relative" }}
      >
        {virtualItems.map((virtualRow) => {
          const item = items[virtualRow.index];
          if (!item) return null;
          return (
            <div
              key={getKey(item)}
              style={{
                position: "absolute",
                top: 0,
                left: 0,
                width: "100%",
                height: `${virtualRow.size}px`,
                transform: `translateY(${virtualRow.start}px)`,
              }}
            >
              {renderRow(item)}
            </div>
          );
        })}
      </div>
    </div>
  );
}

function ProgressRow({
  label,
  active,
  reached,
  paused,
  value,
  suffix,
  loading,
  open,
  onOpenChange,
  children,
}: {
  label: string;
  active: boolean;
  reached: boolean;
  paused?: boolean;
  value?: React.ReactNode;
  suffix?: React.ReactNode;
  loading?: boolean;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  children?: React.ReactNode;
}) {
  const done = reached && !active && !paused;
  const expandable = reached;

  return (
    <Collapsible open={open} onOpenChange={onOpenChange}>
      <CollapsibleTrigger asChild disabled={!expandable}>
        <button
          type="button"
          className={`flex w-full items-center gap-2 px-3 py-2 text-sm text-left ${
            expandable ? "cursor-pointer hover:bg-accent/50" : ""
          } ${
            active ? "bg-blue-500/5" : reached ? "" : "text-muted-foreground opacity-50"
          }`}
        >
          <span className="shrink-0">
            {active ? (
              <Loader2 className="size-4 animate-spin text-blue-500" />
            ) : paused ? (
              <PauseCircle className="size-4 text-gray-500" />
            ) : done ? (
              <CheckCircle2 className="size-4 text-emerald-500" />
            ) : (
              <Circle className="size-4" />
            )}
          </span>
          <span className={`flex-1 ${active ? "font-medium" : ""}`}>{label}</span>
          {reached && value && (
            <span className="tabular-nums text-xs text-muted-foreground">
              {value}
              {suffix && <> · {suffix}</>}
            </span>
          )}
          {expandable && (
            <ChevronRight className={`size-4 text-muted-foreground transition-transform ${open ? "rotate-90" : ""}`} />
          )}
        </button>
      </CollapsibleTrigger>
      {expandable && (
        <CollapsibleContent>
          <div className="border-t bg-muted/30">
            {loading ? (
              <div className="flex items-center justify-center py-4">
                <Loader2 className="size-4 animate-spin text-muted-foreground" />
              </div>
            ) : children}
          </div>
        </CollapsibleContent>
      )}
    </Collapsible>
  );
}

function CreateDialog({ onSuccess }: { onSuccess: () => void }) {
  const [collection, setCollection] = useState<string | null>(null);
  const [did, setDid] = useState("");
  const [open, setOpen] = useState(false);
  const [recordLexicons, setRecordLexicons] = useState<string[]>([]);

  useEffect(() => {
    if (open) {
      getLexicons()
        .then((lexicons) =>
          setRecordLexicons(
            lexicons
              .filter((l) => l.lexicon_type === "record")
              .map((l) => l.id)
              .sort(),
          ),
        )
        .catch(() => {});
    }
  }, [open]);

  async function handleCreate() {
    try {
      await createBackfillJob({
        collection: collection || undefined,
        did: did || undefined,
      });
      toast.success("Backfill job created");
      setCollection(null);
      setDid("");
      setOpen(false);
      onSuccess();
    } catch (e: unknown) {
      toastError("Failed to create backfill job", e);
    }
  }

  return (
    <ResponsiveDialog open={open} onOpenChange={setOpen}>
      <ResponsiveDialogTrigger asChild>
        <Button>Create Backfill Job</Button>
      </ResponsiveDialogTrigger>
      <ResponsiveDialogContent
        onInteractOutside={(e) => {
          const target = e.target as HTMLElement;
          if (
            target.closest(
              "[data-slot='combobox-item'], [data-slot='combobox-content']",
            )
          ) {
            e.preventDefault();
          }
        }}
      >
        <ResponsiveDialogHeader>
          <ResponsiveDialogTitle>Create Backfill Job</ResponsiveDialogTitle>
          <ResponsiveDialogDescription>
            Start a backfill for a collection or specific DID. Leave both empty
            to backfill all collections.
          </ResponsiveDialogDescription>
        </ResponsiveDialogHeader>
        <div className="flex flex-col gap-4">
          <div className="flex flex-col gap-2">
            <Label>Collection (optional)</Label>
            <Combobox
              value={collection}
              onValueChange={setCollection}
              items={recordLexicons}
            >
              <ComboboxInput
                placeholder="Select or type a collection..."
                showClear
              />
              <ComboboxContent>
                <ComboboxEmpty>No matching lexicons.</ComboboxEmpty>
                <ComboboxList>
                  {(item: string) => (
                    <ComboboxItem key={item} value={item}>
                      {item}
                    </ComboboxItem>
                  )}
                </ComboboxList>
              </ComboboxContent>
            </Combobox>
          </div>
          <div className="flex flex-col gap-2">
            <Label htmlFor="bf-did">DID (optional)</Label>
            <Input
              id="bf-did"
              value={did}
              onChange={(e) => setDid(e.target.value)}
              placeholder="did:plc:..."
            />
          </div>
        </div>
        <ResponsiveDialogFooter>
          <ResponsiveDialogClose asChild>
            <Button variant="outline">Cancel</Button>
          </ResponsiveDialogClose>
          <Button onClick={handleCreate}>Create</Button>
        </ResponsiveDialogFooter>
      </ResponsiveDialogContent>
    </ResponsiveDialog>
  );
}
