"use client"

import { useEffect, useState } from "react"

import { SiteHeader } from "@/components/site-header"
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"
import { getJetstreamStatus, type JetstreamStatus } from "@/lib/api"

interface ConfigInfo {
  version: string
  public_url: string
  database_backend: string
  jetstream_url: string
  relay_url: string
  plc_url: string
}

type Row = { label: string; value: string | undefined }

/** A worker whose job has run this long is probably retrying it. */
const RETRYING_AFTER_MS = 30_000
const POLL_MS = 5_000

function formatLag(ms: number | null): string | undefined {
  if (ms === null) return undefined
  if (ms < 1000) return `${ms} ms`
  if (ms < 60_000) return `${(ms / 1000).toFixed(1)} s`
  if (ms < 3_600_000) return `${Math.round(ms / 60_000)} min`
  return `${(ms / 3_600_000).toFixed(1)} h`
}

function formatWorkers(status: JetstreamStatus, now: number): string | undefined {
  if (status.worker_queue_depths.length === 0) return undefined
  return status.worker_queue_depths
    .map((depth, i) => {
      const since = status.worker_busy_since_ms[i] ?? null
      return since !== null && now - since > RETRYING_AFTER_MS
        ? `${depth} (retrying)`
        : String(depth)
    })
    .join(" · ")
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
  )
}
export default function AboutPage() {
  const [config, setConfig] = useState<ConfigInfo | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [jetstream, setJetstream] = useState<{
    status: JetstreamStatus
    fetchedAt: number
  } | null>(null)

  useEffect(() => {
    fetch(`${process.env.NEXT_PUBLIC_BASE_PATH || ""}/config`, { credentials: "same-origin" })
      .then((r) => {
        if (!r.ok) throw new Error("Failed to load config")
        return r.json()
      })
      .then(setConfig)
      .catch((e) => setError(e.message))
  }, [])

  // Needs stats:read; without it the card is simply not shown. Polls while
  // the page is visible.
  useEffect(() => {
    let timer: ReturnType<typeof setInterval> | undefined
    let cancelled = false

    const poll = () => {
      getJetstreamStatus()
        .then((status) => {
          if (!cancelled) setJetstream({ status, fetchedAt: Date.now() })
        })
        .catch(() => {
          if (!cancelled) setJetstream(null)
        })
    }
    const start = () => {
      if (timer !== undefined) return
      poll()
      timer = setInterval(poll, POLL_MS)
    }
    const stop = () => {
      if (timer === undefined) return
      clearInterval(timer)
      timer = undefined
    }
    const onVisibility = () => (document.hidden ? stop() : start())

    if (!document.hidden) start()
    document.addEventListener("visibilitychange", onVisibility)
    return () => {
      cancelled = true
      stop()
      document.removeEventListener("visibilitychange", onVisibility)
    }
  }, [])

  const rows: Row[] = config
    ? [
        { label: "Version", value: config.version },
        { label: "Public URL", value: config.public_url },
        { label: "Database", value: config.database_backend },
        { label: "Jetstream", value: config.jetstream_url },
        { label: "Relay", value: config.relay_url },
        { label: "PLC Directory", value: config.plc_url },
      ]
    : []

  const jetstreamRows: Row[] = jetstream
    ? [
        { label: "Protocol", value: jetstream.status.protocol ?? undefined },
        { label: "Host", value: jetstream.status.host ?? undefined },
        {
          label: "Connected",
          value: jetstream.status.connected ? "Yes" : "No",
        },
        {
          label: "Compression",
          value: jetstream.status.compression
            ? `zstd, dictionary ${jetstream.status.dictionary_id}`
            : "Off",
        },
        {
          label: "Applied seq",
          value: jetstream.status.applied_seq?.toString(),
        },
        { label: "Lag", value: formatLag(jetstream.status.lag_ms) },
        {
          label: "Worker queues",
          value: formatWorkers(jetstream.status, jetstream.fetchedAt),
        },
      ]
    : []

  return (
    <>
      <SiteHeader title="About" />
      <div className="flex flex-1 flex-col gap-4 p-4 md:p-6 max-w-3xl">
        {error && <p className="text-destructive text-sm">{error}</p>}

        <Card>
          <CardHeader>
            <CardTitle>HappyView</CardTitle>
            <CardDescription>
              ATProto AppView instance information
            </CardDescription>
          </CardHeader>
          <CardContent>
            {config ? (
              <Rows rows={rows} />
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
  )
}
