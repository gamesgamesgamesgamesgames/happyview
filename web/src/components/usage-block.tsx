"use client"

import {
  Fragment,
  type ComponentProps,
  type ReactNode,
  useEffect,
  useState,
} from "react"
import { jsx, jsxs } from "react/jsx-runtime"
import { toJsxRuntime } from "hast-util-to-jsx-runtime"
// The full bundle rather than `shiki/bundle/web`, which carries no Lua
// grammar — the one language that most needs highlighting here. Grammars load
// on demand, so the tab a reader opens is the only one fetched.
import { codeToHast, type BundledLanguage } from "shiki"
import { Check, Copy } from "lucide-react"

import { SCRIPT_LANGUAGES, bindingFor } from "@/lib/script-usage"
import { Button } from "@/components/ui/button"
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs"

async function highlight(code: string, lang: string): Promise<ReactNode> {
  const hast = await codeToHast(code, {
    lang: lang as BundledLanguage,
    themes: { light: "github-light", dark: "github-dark" },
    // The `.shiki` rules in globals.css read the per-theme custom properties
    // this emits, so the block follows the dashboard's own theme switch.
    defaultColor: false,
  })

  return toJsxRuntime(hast, {
    Fragment,
    jsx,
    jsxs,
    components: {
      pre: (props: ComponentProps<"pre">) => <pre {...props} />,
      code: (props: ComponentProps<"code">) => (
        <code className="font-mono text-sm whitespace-pre" {...props} />
      ),
    },
  }) as ReactNode
}

/** The highlighted line, or null until the grammar has loaded. */
function useHighlighted(code: string, grammar: string): ReactNode | null {
  const key = `${grammar}:${code}`
  // The result carries the input it was produced from, so a result that
  // arrives after the input changed is ignored rather than cleared on the way
  // in — clearing would be a setState in the effect body.
  const [done, setDone] = useState<{ key: string; nodes: ReactNode } | null>(
    null,
  )

  useEffect(() => {
    let cancelled = false
    void highlight(code, grammar)
      .then((nodes) => {
        if (!cancelled) setDone({ key, nodes })
      })
      // An unavailable grammar costs colour, not the line: the plain fallback
      // is the same text.
      .catch(() => {})
    return () => {
      cancelled = true
    }
  }, [code, grammar, key])

  return done?.key === key ? done.nodes : null
}

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
          <code className="font-mono text-sm whitespace-pre">{code}</code>
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
