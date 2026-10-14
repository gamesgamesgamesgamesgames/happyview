"use client"

import { useState } from "react"
import { IconArrowLeft } from "@tabler/icons-react"
import { AlertTriangle, ChevronDown } from "lucide-react"
import Link from "next/link"

import { useRestart } from "@/lib/restart-context"
import { Button } from "@/components/ui/button"
import { Separator } from "@/components/ui/separator"
import { SidebarTrigger } from "@/components/ui/sidebar"
import { ThemeToggle } from "@/components/theme-toggle"

export function SiteHeader({
  title,
  backHref,
  actions,
}: {
  title: string
  backHref?: string
  /** Page-level links that would crowd a toolbar, set beside the theme toggle. */
  actions?: React.ReactNode
}) {
  const { reasons } = useRestart()
  const [expanded, setExpanded] = useState(false)

  return (
    <header className="sticky top-0 z-10 bg-background">
      <div className="flex h-(--header-height) shrink-0 items-center gap-2 border-b transition-[width,height] ease-linear group-has-data-[collapsible=icon]/sidebar-wrapper:h-(--header-height)">
        <div className="flex w-full items-center gap-1 px-4 lg:gap-2 lg:px-6">
          <SidebarTrigger className="-ml-1" />
          <Separator
            orientation="vertical"
            className="mx-2 data-[orientation=vertical]:h-4"
          />
          {backHref && (
            <Button variant="ghost" size="icon" className="-ml-1 size-7" asChild>
              <Link href={backHref}>
                <IconArrowLeft className="size-4" />
                <span className="sr-only">Back</span>
              </Link>
            </Button>
          )}
          <h1 className="text-base font-medium">{title}</h1>
          <div className="ml-auto flex items-center gap-2">
            {actions}
            <ThemeToggle />
          </div>
        </div>
      </div>
      {reasons.length > 0 && (
        <div className="border-b bg-amber-500/10">
          <button
            type="button"
            onClick={() => setExpanded((v) => !v)}
            className="flex w-full items-center gap-2 px-4 py-2 text-xs text-amber-600 hover:bg-amber-500/10 dark:text-amber-400 lg:px-6"
          >
            <AlertTriangle className="size-3.5 shrink-0" />
            <span className="font-medium">Restart required</span>
            <ChevronDown className={`ml-auto size-3.5 transition-transform ${expanded ? "rotate-180" : ""}`} />
          </button>
          {expanded && (
            <ul className="px-4 pb-2 text-xs text-amber-600 dark:text-amber-400 lg:px-6">
              {reasons.map((reason, i) => (
                <li key={i} className="flex items-start gap-2 py-0.5">
                  <span className="mt-1 size-1 shrink-0 rounded-full bg-current" />
                  {reason}
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
    </header>
  )
}
