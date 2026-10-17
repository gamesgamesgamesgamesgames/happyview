"use client"

import ReactMarkdown from "react-markdown"
import remarkGfm from "remark-gfm"

import { cn } from "@/lib/utils"

/**
 * CommonMark written by a plugin's publisher and shown to an operator.
 *
 * `at.happyproto.plugin.profile` states the handling as a requirement: raw
 * HTML is not rendered, and a link scheme the renderer does not recognise is
 * refused. Both hold here by construction — no `rehype-raw`, so embedded HTML
 * stays text, and react-markdown's default URL transform passes only the safe
 * schemes — so this component exists to keep that true in one place rather
 * than at each call site.
 */
export function PublishedMarkdown({
  children,
  className,
}: {
  children: string
  className?: string
}) {
  return (
    <div
      className={cn(
        "prose prose-sm dark:prose-invert max-w-none",
        // Prose sets its own mono styling for code; keep it from outgrowing
        // the surrounding type.
        "prose-pre:text-xs prose-code:text-xs",
        className,
      )}
    >
      <ReactMarkdown remarkPlugins={[remarkGfm]}>{children}</ReactMarkdown>
    </div>
  )
}
