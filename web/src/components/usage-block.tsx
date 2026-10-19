"use client"

import { useState } from "react"
import { Check, Copy } from "lucide-react"

import { CODE_CLASS, useHighlighted } from "@/lib/highlight"
import { SCRIPT_LANGUAGES, bindingFor } from "@/lib/script-usage"
import { Button } from "@/components/ui/button"
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs"

function CopyButton({ value, label }: { value: string; label: string }) {
  const [copied, setCopied] = useState(false)
  return (
    <Button
      variant="ghost"
      size="sm"
      className="h-7 gap-1.5 px-2 text-xs text-muted-foreground hover:text-foreground"
      aria-label={label}
      onClick={() => {
        void navigator.clipboard.writeText(value).then(() => {
          setCopied(true)
          setTimeout(() => setCopied(false), 1500)
        })
      }}
    >
      {copied ? <Check className="size-3.5" /> : <Copy className="size-3.5" />}
      {copied ? "Copied" : "Copy"}
    </Button>
  )
}

function UsageLine({ code, grammar }: { code: string; grammar: string }) {
  const nodes = useHighlighted(code, grammar)
  return (
    <div className="shiki-on-surface overflow-x-auto px-4 py-3">
      {nodes ?? (
        <pre>
          <code className={CODE_CLASS}>{code}</code>
        </pre>
      )}
    </div>
  )
}

/**
 * How to reach this library from a script, one tab per language. The
 * namespace is the same in every language; only the line around it differs.
 *
 * Tabs and code share one panel rather than floating above it, and the tabs
 * are controlled so the single copy button in the panel's header can copy
 * whichever line is showing.
 */
export function UsageBlock({ namespace }: { namespace: string }) {
  const binding = bindingFor(namespace)
  const [active, setActive] = useState(SCRIPT_LANGUAGES[0].id)
  const current =
    SCRIPT_LANGUAGES.find((language) => language.id === active) ??
    SCRIPT_LANGUAGES[0]

  return (
    <div className="flex flex-col gap-2">
      <h3 className="text-sm font-medium">Use it</h3>
      <Tabs value={active} onValueChange={setActive} className="gap-0">
        <div className="overflow-hidden rounded-lg border">
          <div className="flex items-center justify-between gap-2 border-b bg-muted/40 px-2 py-1.5">
            <TabsList variant="line" className="h-auto">
              {SCRIPT_LANGUAGES.map((language) => (
                <TabsTrigger key={language.id} value={language.id}>
                  {language.label}
                </TabsTrigger>
              ))}
            </TabsList>
            <CopyButton
              value={current.importLine(binding, namespace)}
              label={`Copy the ${current.label} line`}
            />
          </div>
          {SCRIPT_LANGUAGES.map((language) => (
            <TabsContent key={language.id} value={language.id}>
              <UsageLine
                code={language.importLine(binding, namespace)}
                grammar={language.grammar}
              />
            </TabsContent>
          ))}
        </div>
      </Tabs>
    </div>
  )
}
