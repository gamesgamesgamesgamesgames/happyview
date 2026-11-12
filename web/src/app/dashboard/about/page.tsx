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
import {
  OfferLinks,
  useUpdateStatus,
} from "@/components/happyview-update-notice"
import type { UpdateStatus } from "@/lib/api"

interface ConfigInfo {
  version: string
  public_url: string
  database_backend: string
  jetstream_url: string
  relay_url: string
  plc_url: string
}

/** What the update check knows, under the version on the About card. */
function UpdateLine({ status }: { status: UpdateStatus | null }) {
  if (!status) return null
  const checked = status.checkedAt
    ? `Last checked ${new Date(status.checkedAt).toLocaleString()}`
    : null
  let summary: React.ReactNode
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
      ) : null
      break
    case "current":
      summary = <>Up to date on the {status.channel} channel.</>
      break
    case "unknown":
      summary = <>Not checked yet.</>
      break
    case "disabled":
      summary = (
        <>Update checks are off (UPDATE_CHECK or PLUGIN_REGISTRY_URL).</>
      )
      break
    case "dev":
      summary = <>A development build, so there are no updates to check for.</>
      break
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
  )
}

export default function AboutPage() {
  const [config, setConfig] = useState<ConfigInfo | null>(null)
  const [error, setError] = useState<string | null>(null)
  const updateStatus = useUpdateStatus()

  useEffect(() => {
    fetch(`${process.env.NEXT_PUBLIC_BASE_PATH || ""}/config`, { credentials: "same-origin" })
      .then((r) => {
        if (!r.ok) throw new Error("Failed to load config")
        return r.json()
      })
      .then(setConfig)
      .catch((e) => setError(e.message))
  }, [])

  const rows: { label: string; value: string | undefined }[] = config
    ? [
        { label: "Version", value: config.version },
        { label: "Public URL", value: config.public_url },
        { label: "Database", value: config.database_backend },
        { label: "Jetstream", value: config.jetstream_url },
        { label: "Relay", value: config.relay_url },
        { label: "PLC Directory", value: config.plc_url },
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
              <div className="flex flex-col gap-4">
                <dl className="grid grid-cols-[auto_1fr] gap-x-6 gap-y-3 text-sm">
                  {rows.map((row) => (
                    <div key={row.label} className="contents">
                      <dt className="text-muted-foreground font-medium">
                        {row.label}
                      </dt>
                      <dd className="font-mono">{row.value ?? "—"}</dd>
                    </div>
                  ))}
                </dl>
                <UpdateLine status={updateStatus} />
              </div>
            ) : (
              !error && (
                <p className="text-muted-foreground text-sm">Loading...</p>
              )
            )}
          </CardContent>
        </Card>
      </div>
    </>
  )
}
