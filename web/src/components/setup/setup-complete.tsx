"use client"

import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card"
import { Button } from "@/components/ui/button"
import { CheckCircle2, FileUp, Settings, LayoutDashboard } from "lucide-react"
import { useRouter } from "next/navigation"
import { completeSetup } from "@/lib/api"
import { docsUrl } from "@/lib/docs"
import { useEffect, useState } from "react"

interface SetupCompleteProps { identityMode: string | null }

const MODE_LABELS: Record<string, string> = {
  did_web: "Domain identity (did:web)",
  did_plc: "Network identity (did:plc)",
  attach_account: "Linked AT Protocol account",
  not_exposed: "Skipped — using built-in auth",
}

export function SetupComplete({ identityMode }: SetupCompleteProps) {
  const router = useRouter()
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    if (identityMode === "not_exposed") {
      completeSetup().catch((e) => {
        setError(e instanceof Error ? e.message : "Failed to finalize setup. You can retry from the dashboard settings.")
      })
    }
  }, [identityMode])

  const modeLabel = identityMode ? MODE_LABELS[identityMode] ?? identityMode : "Configured"

  return (
    <Card>
      <CardHeader className="text-center">
        <div className="flex justify-center mb-4">
          <div className="flex h-12 w-12 items-center justify-center rounded-full bg-primary/10">
            <CheckCircle2 className="h-6 w-6 text-primary" />
          </div>
        </div>
        <CardTitle>Your AppView is ready</CardTitle>
        <CardDescription>
          {identityMode === "not_exposed"
            ? "HappyView is running with built-in auth. You can configure a service identity anytime from settings."
            : "Your service identity is configured and your AppView is ready to accept requests from the AT Protocol network."}
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-6">
        <dl className="rounded-lg border p-4">
          <dt className="text-muted-foreground text-xs font-medium">Service identity</dt>
          <dd className="mt-1 text-sm font-medium">{modeLabel}</dd>
          <dd className="mt-1">
            <a href={docsUrl("/getting-started/service-identity")} target="_blank" rel="noopener noreferrer" className="text-primary text-xs underline underline-offset-4 hover:text-primary/80">What does this mean?</a>
          </dd>
        </dl>

        {error && <p role="alert" className="text-destructive text-sm">{error}</p>}

        <div className="space-y-2">
          <p className="text-muted-foreground text-xs font-medium">Next steps</p>
          <div className="grid gap-2">
            <Button
              variant="outline"
              className="justify-start gap-3 h-auto py-3"
              onClick={() => router.push("/dashboard/lexicons")}
            >
              <FileUp className="h-4 w-4 shrink-0" />
              <div className="text-left">
                <div className="text-sm font-medium">Upload a lexicon</div>
                <div className="text-muted-foreground text-xs">Define your first schema to start indexing records.</div>
              </div>
            </Button>
            <Button
              variant="outline"
              className="justify-start gap-3 h-auto py-3"
              onClick={() => router.push("/dashboard/settings")}
            >
              <Settings className="h-4 w-4 shrink-0" />
              <div className="text-left">
                <div className="text-sm font-medium">Review settings</div>
                <div className="text-muted-foreground text-xs">Configure your instance name, policies, and service entries.</div>
              </div>
            </Button>
          </div>
        </div>

        <div className="flex justify-center pt-2">
          <Button onClick={() => router.push("/dashboard")} className="gap-2">
            <LayoutDashboard className="h-4 w-4" />
            Go to Dashboard
          </Button>
        </div>
      </CardContent>
    </Card>
  )
}
