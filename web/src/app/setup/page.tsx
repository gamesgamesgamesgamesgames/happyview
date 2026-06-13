"use client"

import { useAuth } from "@/lib/auth-context"
import { useRouter } from "next/navigation"
import { useEffect } from "react"
import { SetupWizard } from "@/components/setup/setup-wizard"

export default function SetupPage() {
  const { did } = useAuth()
  const router = useRouter()

  useEffect(() => {
    if (!did) {
      router.replace("/login")
    }
  }, [did, router])

  if (!did) return null

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
