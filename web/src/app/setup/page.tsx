"use client"

import { useRouter } from "next/navigation"
import { useEffect, useState } from "react"
import { getSetupStatus } from "@/lib/api"
import { SetupWizard } from "@/components/setup/setup-wizard"

export default function SetupPage() {
  const router = useRouter()
  const [ready, setReady] = useState(false)

  useEffect(() => {
    getSetupStatus()
      .then((status) => {
        if (status.setup_complete) {
          router.replace("/dashboard")
        } else {
          setReady(true)
        }
      })
      .catch(() => {
        setReady(true)
      })
  }, [router])

  if (!ready) return null

  return (
    <div className="bg-background flex min-h-svh flex-col items-center justify-center p-6 md:p-10">
      <div className="w-full max-w-4xl">
        <div className="mb-8 text-center">
          <h1 className="text-3xl font-bold">Setup</h1>
          <p className="text-muted-foreground mt-2">Configure your HappyView instance</p>
        </div>
        <SetupWizard />
      </div>
    </div>
  )
}
