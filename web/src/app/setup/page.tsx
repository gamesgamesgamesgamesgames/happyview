"use client"

import { useRouter } from "next/navigation"
import { useEffect, useState } from "react"
import { getSetupStatus } from "@/lib/api"
import { useAuth } from "@/lib/auth-context"
import { SetupWizard } from "@/components/setup/setup-wizard"
import { Skeleton } from "@/components/ui/skeleton"

export default function SetupPage() {
  const router = useRouter()
  const { did } = useAuth()
  const [ready, setReady] = useState(false)
  const [backendError, setBackendError] = useState(false)

  useEffect(() => {
    if (!did) {
      router.replace("/login")
      return
    }

    getSetupStatus()
      .then((status) => {
        if (status.setup_complete) {
          router.replace("/dashboard")
        } else {
          setReady(true)
        }
      })
      .catch(() => {
        setBackendError(true)
        setReady(true)
      })
  }, [did, router])

  return (
    <div className="bg-background flex min-h-svh flex-col items-center justify-center p-6 md:p-10">
      <div className="w-full max-w-4xl">
        <div className="mb-8 text-center">
          <h1 className="text-xl font-semibold">Welcome to HappyView</h1>
          <p className="text-muted-foreground mt-2">Let&apos;s get your AppView ready for the AT Protocol network.</p>
        </div>
        {backendError && (
          <div role="alert" className="mb-4 rounded-lg border border-destructive/20 bg-destructive/10 p-3 text-sm text-destructive">
            Could not reach the backend. Setup steps may not save correctly.
          </div>
        )}
        {ready ? <SetupWizard /> : (
          <div className="space-y-4" role="status" aria-label="Loading setup">
            <Skeleton className="h-10 w-full" />
            <Skeleton className="h-64 w-full rounded-xl" />
          </div>
        )}
      </div>
    </div>
  )
}
