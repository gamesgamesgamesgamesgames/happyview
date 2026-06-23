"use client"

import { HelpCircle } from "lucide-react"
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "@/components/ui/tooltip"

interface HelpTipProps {
  label: string
  href?: string
}

export function HelpTip({ label, href }: HelpTipProps) {
  return (
    <TooltipProvider>
      <Tooltip>
        <TooltipTrigger asChild>
          {href ? (
            <a
              href={href}
              target="_blank"
              rel="noopener noreferrer"
              className="text-muted-foreground hover:text-foreground inline-flex align-middle transition-colors"
              aria-label={label}
            >
              <HelpCircle className="h-3.5 w-3.5" />
            </a>
          ) : (
            <span className="text-muted-foreground inline-flex align-middle cursor-help" aria-label={label}>
              <HelpCircle className="h-3.5 w-3.5" />
            </span>
          )}
        </TooltipTrigger>
        <TooltipContent side="top" className="max-w-64">
          {label}
        </TooltipContent>
      </Tooltip>
    </TooltipProvider>
  )
}
