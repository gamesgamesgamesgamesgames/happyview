"use client"

import { useEffect, useState } from "react"
import { useRouter } from "next/navigation"

import { getSetupStatus } from "@/lib/api"
import { useAuth } from "@/lib/auth-context"
import { useConfig } from "@/lib/config-context"
import { AppSidebar } from "@/components/app-sidebar"
import { PluginUpdateProvider } from "@/components/plugin-update-provider"
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
    getSetupStatus()
      .then((status) => {
        if (!status.setup_complete) {
          router.replace("/setup")
        } else if (!did) {
          router.replace("/login")
        } else {
          setSetupChecked(true)
        }
      })
      .catch(() => {
        if (!did) {
          router.replace("/login")
        } else {
          setSetupChecked(true)
        }
      })
  }, [did, router])

  useEffect(() => {
    document.title = app_name ? `${app_name} Admin` : "HappyView Admin"
  }, [app_name])

  if (!did || !setupChecked) return null

  return (
    <PluginUpdateProvider>
      <RestartProvider>
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
      </RestartProvider>
      <Toaster />
    </PluginUpdateProvider>
  )
}
