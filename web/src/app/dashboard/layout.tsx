"use client"

import { useEffect, useState } from "react"
import { useRouter } from "next/navigation"

import { getSetupStatus } from "@/lib/api"
import { useAuth } from "@/lib/auth-context"
import { useConfig } from "@/lib/config-context"
import { AppSidebar } from "@/components/app-sidebar"
import { PluginUpdateProvider } from "@/components/plugin-update-provider"
import { VacuumPromptProvider } from "@/components/vacuum-prompt-provider"
import { RestartProvider } from "@/lib/restart-context"
import { SidebarInset, SidebarProvider } from "@/components/ui/sidebar"
import { Toaster } from "@/components/ui/sonner"

export default function DashboardLayout({
  children,
}: {
  children: React.ReactNode
}) {
  const { did } = useAuth()
  const { app_name } = useConfig()
  const router = useRouter()
  const [setupChecked, setSetupChecked] = useState(false)

  useEffect(() => {
    if (!did) {
      router.replace("/login")
      return
    }

    getSetupStatus()
      .then((status) => {
        if (!status.setup_complete) {
          router.replace("/setup")
        } else {
          setSetupChecked(true)
        }
      })
      .catch(() => {
        setSetupChecked(true)
      })
  }, [did, router])

  useEffect(() => {
    document.title = app_name ? `${app_name} Admin` : "HappyView Admin"
  }, [app_name])

  if (!did || !setupChecked) return null

  return (
    <PluginUpdateProvider>
      <RestartProvider>
        {/* VacuumPromptProvider calls useRestart() to register the "vacuum
            scheduled" restart reason, so it must be nested INSIDE
            RestartProvider — a provider can't consume a context that is its
            own descendant; React resolves context by tree position, not
            call order, so getting this nesting backwards silently falls
            through to the context's no-op default instead of erroring. */}
        <VacuumPromptProvider>
          <SidebarProvider
            style={
              {
                "--sidebar-width": "calc(var(--spacing) * 72)",
                "--header-height": "calc(var(--spacing) * 12)",
              } as React.CSSProperties
            }
          >
            <AppSidebar variant="inset" />
            <SidebarInset>{children}</SidebarInset>
          </SidebarProvider>
        </VacuumPromptProvider>
        <Toaster />
      </RestartProvider>
    </PluginUpdateProvider>
  )
}
