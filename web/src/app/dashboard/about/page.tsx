"use client";

import { useEffect, useState } from "react";

import { SiteHeader } from "@/components/site-header";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import {
  OfferLinks,
  useUpdateStatus,
} from "@/components/happyview-update-notice";
import {
  ApiError,
  getJetstreamStatus,
  type JetstreamStatus,
  type UpdateStatus,
} from "@/lib/api";

interface ConfigInfo {
  version: string;
  public_url: string;
  database_backend: string;
  jetstream_url: string;
  relay_url: string;
  plc_url: string;
}

type Row = { label: string; value: string | undefined };

/** A worker whose job has run this long is probably retrying it. */
const RETRYING_AFTER_MS = 30_000;
const POLL_MS = 5_000;

function formatLag(ms: number | null): string | undefined {
  if (ms === null) return undefined;
  if (ms < 1000) return `${ms} ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(1)} s`;
  if (ms < 3_600_000) return `${Math.round(ms / 60_000)} min`;
  return `${(ms / 3_600_000).toFixed(1)} h`;
}

function formatWorkers(status: JetstreamStatus): string | undefined {
  if (status.worker_queue_depths.length === 0) return undefined;
  return status.worker_queue_depths
    .map((depth, i) => {
      const since = status.worker_busy_since_ms[i] ?? null;
      return since !== null && status.now_ms - since > RETRYING_AFTER_MS
        ? `${depth} (retrying)`
        : String(depth);
    })
    .join(" · ");
}

function Rows({ rows }: { rows: Row[] }) {
  return (
    <dl className="grid grid-cols-[auto_1fr] gap-x-6 gap-y-3 text-sm">
      {rows.map((row) => (
        <div key={row.label} className="contents">
          <dt className="text-muted-foreground font-medium">{row.label}</dt>
          <dd className="font-mono">{row.value ?? "—"}</dd>
        </div>
      ))}
    </dl>
  );
}

/** What the update check knows, under the version on the About card. */
function UpdateLine({ status }: { status: UpdateStatus | null }) {
  if (!status) return null;
  const checked = status.checkedAt
    ? `Last checked ${new Date(status.checkedAt).toLocaleString()}`
    : null;
  let summary: React.ReactNode;
  switch (status.state) {
    case "available":
      summary = status.available ? (
        <>
          <span className="text-foreground font-medium">
            {status.available.version} is available on the{" "}
            {status.available.channel} channel.
          </span>{" "}
          <OfferLinks offer={status.available} managed={status.managed} />
        </>
      ) : null;
      break;
    case "current":
      summary = <>Up to date on the {status.channel} channel.</>;
      break;
    case "unknown":
      summary = status.lastError ? (
        <>No check has succeeded yet.</>
      ) : (
        <>Not checked yet.</>
      );
      break;
    case "disabled":
      summary = <>Update checks are off (UPDATE_CHECK=off).</>;
      break;
    case "dev":
      summary = <>A development build, so there are no updates to check for.</>;
      break;
  }
  return (
    <div className="text-muted-foreground flex flex-col gap-1 text-sm">
      <p>{summary}</p>
      {status.otherLine ? (
        <p>
          HappyView {status.otherLine.version} is out on the{" "}
          {status.otherLine.channel} channel.{" "}
          <OfferLinks offer={status.otherLine} managed={status.managed} />
        </p>
      ) : null}
      {status.lastError ? (
        <p>The last check failed: {status.lastError}</p>
      ) : null}
      {checked ? <p>{checked}</p> : null}
    </div>
  );
}

export default function AboutPage() {
  const [config, setConfig] = useState<ConfigInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [jetstream, setJetstream] = useState<JetstreamStatus | null>(null);
  const updateStatus = useUpdateStatus();

  useEffect(() => {
    fetch(`${process.env.NEXT_PUBLIC_BASE_PATH || ""}/config`, {
      credentials: "same-origin",
    })
      .then((r) => {
        if (!r.ok) throw new Error("Failed to load config");
        return r.json();
      })
      .then(setConfig)
      .catch((e) => setError(e.message));
  }, []);

  // Needs stats:read; without it the card is simply not shown. Polls while
  // the page is visible.
  useEffect(() => {
    let timer: ReturnType<typeof setInterval> | undefined;
    let cancelled = false;

    const poll = () => {
      getJetstreamStatus()
        .then((status) => {
          if (!cancelled) setJetstream(status);
        })
        .catch((e) => {
          // Keep the last good status through a transient failure; hide the
          // card only when the user cannot read it at all.
          if (
            !cancelled &&
            e instanceof ApiError &&
            (e.status === 403 || e.status === 404)
          ) {
            setJetstream(null);
          }
        });
    };
    const start = () => {
      if (timer !== undefined) return;
      poll();
      timer = setInterval(poll, POLL_MS);
    };
    const stop = () => {
      if (timer === undefined) return;
      clearInterval(timer);
      timer = undefined;
    };
    const onVisibility = () => (document.hidden ? stop() : start());

    if (!document.hidden) start();
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      cancelled = true;
      stop();
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, []);

  const rows: Row[] = config
    ? [
        { label: "Version", value: config.version },
        { label: "Public URL", value: config.public_url },
        { label: "Database", value: config.database_backend },
        { label: "Jetstream", value: config.jetstream_url },
        { label: "Relay", value: config.relay_url },
        { label: "PLC Directory", value: config.plc_url },
      ]
    : [];

  const jetstreamRows: Row[] = jetstream
    ? [
        { label: "Protocol", value: jetstream.protocol ?? undefined },
        { label: "Host", value: jetstream.host ?? undefined },
        {
          label: "Connected",
          value: jetstream.connected ? "Yes" : "No",
        },
        {
          label: "Compression",
          value: jetstream.compression
            ? `zstd, dictionary ${jetstream.dictionary_id}`
            : "Off",
        },
        {
          label: "Applied seq",
          value: jetstream.applied_seq?.toString(),
        },
        { label: "Lag", value: formatLag(jetstream.lag_ms) },
        {
          label: "Worker queues",
          value: formatWorkers(jetstream),
        },
      ]
    : [];

  return (
    <>
      <SiteHeader title="About" />
      <div className="flex flex-1 flex-col gap-4 p-4 md:p-6 max-w-3xl">
        {error && <p className="text-destructive text-sm">{error}</p>}

        <Card>
          <CardHeader>
            <CardTitle>HappyView</CardTitle>
          </CardHeader>
          <CardContent>
            {config ? (
              <div className="flex flex-col gap-4">
                <Rows rows={rows} />
                <UpdateLine status={updateStatus} />
              </div>
            ) : (
              !error && (
                <p className="text-muted-foreground text-sm">Loading...</p>
              )
            )}
          </CardContent>
        </Card>

        {jetstream && (
          <Card>
            <CardHeader>
              <CardTitle>Jetstream</CardTitle>
              <CardDescription>
                The live event stream this instance indexes from
              </CardDescription>
            </CardHeader>
            <CardContent>
              <Rows rows={jetstreamRows} />
            </CardContent>
          </Card>
        )}
      </div>
    </>
  );
}
