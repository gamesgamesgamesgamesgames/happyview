"use client"

import { useEffect, useState } from "react"
import type { Combobox as ComboboxPrimitive } from "@base-ui/react"
import { Loader2 } from "lucide-react"

import { Avatar, AvatarFallback, AvatarImage } from "@/components/ui/avatar"
import {
  Combobox,
  ComboboxChip,
  ComboboxChips,
  ComboboxChipsInput,
  ComboboxContent,
  ComboboxItem,
  ComboboxList,
  useComboboxAnchor,
} from "@/components/ui/combobox"
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip"
import { searchActors, type TypeaheadActor } from "@/lib/typeahead"
import { cn } from "@/lib/utils"

import {
  splitEntries,
  useAccountTags,
  type AccountTag,
} from "./use-account-tags"

type Option =
  | { kind: "tag"; tag: AccountTag }
  | { kind: "actor"; actor: TypeaheadActor }

// Suggestions never equal tags. If they did, Base UI would treat choosing a
// suggestion for an existing account as deselecting it and remove the tag;
// instead the choice reaches `add`, which ignores accounts already present.
function sameOption(a: Option, b: Option) {
  if (a.kind === "tag" && b.kind === "tag") return a.tag.key === b.tag.key
  if (a.kind === "actor" && b.kind === "actor") return a.actor.did === b.actor.did
  return false
}

function optionLabel(option: Option) {
  return option.kind === "tag" ? option.tag.input : option.actor.handle
}

function AccountChip({ tag }: { tag: AccountTag }) {
  const chip = (
    <ComboboxChip
      data-status={tag.status}
      aria-invalid={tag.status === "error" || undefined}
      className={cn(
        "h-auto min-h-[calc(--spacing(5.5))] py-0.5",
        tag.status === "error" && "bg-destructive/10 text-destructive",
      )}
    >
      {tag.status === "resolving" && (
        <Loader2 className="size-3 animate-spin" aria-hidden />
      )}
      {tag.status === "resolved" && tag.avatar && (
        <Avatar className="size-4">
          <AvatarImage src={tag.avatar} alt="" />
          <AvatarFallback />
        </Avatar>
      )}
      <span className="flex flex-col items-start leading-tight">
        <span>
          {tag.status === "resolved"
            ? tag.handle
              ? `@${tag.handle}`
              : "No verified handle"
            : tag.input}
        </span>
        {tag.status === "resolved" && (
          <span className="text-muted-foreground font-mono text-[10px] font-normal">
            {tag.did}
          </span>
        )}
      </span>
    </ComboboxChip>
  )

  if (tag.status !== "error") return chip

  return (
    <Tooltip>
      <TooltipTrigger asChild>{chip}</TooltipTrigger>
      <TooltipContent>{tag.error}</TooltipContent>
    </Tooltip>
  )
}

export function AccountInput({
  id,
  placeholder = "alice.bsky.social or did:plc:...",
  max,
  onChange,
  onBlockedChange,
}: {
  id: string
  placeholder?: string
  max?: number
  onChange: (dids: string[]) => void
  onBlockedChange: (blocked: boolean) => void
}) {
  const { tags, add, remove } = useAccountTags({ max, onChange, onBlockedChange })
  const anchor = useComboboxAnchor()
  const [query, setQuery] = useState("")
  const [suggestions, setSuggestions] = useState<TypeaheadActor[]>([])
  const [highlighted, setHighlighted] = useState(false)

  const term = query.trim().replace(/^@/, "")
  const searchable = term.length >= 2 && !term.startsWith("did:")
  const full = max !== undefined && tags.length >= max

  useEffect(() => {
    if (!searchable) return
    const controller = new AbortController()
    const timer = setTimeout(() => {
      searchActors(term, controller.signal).then((actors) => {
        if (!controller.signal.aborted) setSuggestions(actors)
      })
    }, 250)
    return () => {
      clearTimeout(timer)
      controller.abort()
    }
  }, [term, searchable])

  const items: Option[] = searchable
    ? suggestions.map((actor) => ({ kind: "actor", actor }))
    : []
  const value: Option[] = tags.map((tag) => ({ kind: "tag", tag }))

  function handleValueChange(
    next: Option[],
    eventDetails: ComboboxPrimitive.Root.ChangeEventDetails,
  ) {
    // Base UI clears the selection (and, in ComboboxChipsInput below, the
    // typed text) on Escape; ignoring escape-key changes here and keeping
    // `value`/`inputValue` controlled restores both on the next render.
    if (eventDetails.reason === "escape-key") return
    const kept = new Set(
      next.flatMap((o) => (o.kind === "tag" ? [o.tag.key] : [])),
    )
    remove(tags.filter((t) => !kept.has(t.key)).map((t) => t.key))
    for (const option of next) {
      if (option.kind === "actor") add([option.actor.handle], option.actor)
    }
    setQuery("")
  }

  function commitTyped() {
    const entries = splitEntries(query)
    if (entries.length === 0) return false
    add(entries)
    setQuery("")
    return true
  }

  return (
    <Combobox
      multiple
      items={items}
      value={value}
      onValueChange={handleValueChange}
      inputValue={query}
      onInputValueChange={(next, eventDetails) => {
        // See the `escape-key` check in `handleValueChange` above.
        if (eventDetails.reason === "escape-key") return
        setQuery(next)
      }}
      filter={null}
      itemToStringLabel={optionLabel}
      isItemEqualToValue={sameOption}
      onItemHighlighted={(item) => setHighlighted(item !== undefined)}
      // Base UI stops syncing the highlight while closed, so a stale highlight
      // would otherwise swallow Enter after Escape.
      onOpenChange={(open) => {
        if (!open) setHighlighted(false)
      }}
    >
      <ComboboxChips ref={anchor}>
        {tags.map((tag) => (
          <AccountChip key={tag.key} tag={tag} />
        ))}
        <ComboboxChipsInput
          id={id}
          placeholder={tags.length === 0 ? placeholder : undefined}
          className={cn(full && "hidden")}
          onKeyDown={(e) => {
            if (e.key === ",") {
              // A comma is a separator, never part of an entry.
              e.preventDefault()
              commitTyped()
            } else if (e.key === "Enter" && !highlighted) {
              if (commitTyped()) e.preventDefault()
            }
          }}
          onPaste={(e) => {
            const text = e.clipboardData.getData("text")
            if (/[\s,]/.test(text.trim())) {
              e.preventDefault()
              add(splitEntries(text))
            }
          }}
          onBlur={(e) => {
            // Clicking a suggestion moves focus into the popup before the
            // click lands; committing the partial text here would replace the
            // suggestion list out from under that click.
            const next = e.relatedTarget
            if (
              next instanceof Element &&
              next.closest('[data-slot="combobox-content"]')
            ) {
              return
            }
            commitTyped()
          }}
        />
      </ComboboxChips>
      <ComboboxContent anchor={anchor} className="data-empty:hidden">
        <ComboboxList>
          {(option: Option) =>
            option.kind === "actor" ? (
              <ComboboxItem key={option.actor.did} value={option}>
                <Avatar className="size-6">
                  <AvatarImage src={option.actor.avatar} alt="" />
                  <AvatarFallback>
                    {option.actor.handle.slice(0, 1).toUpperCase()}
                  </AvatarFallback>
                </Avatar>
                <span className="flex min-w-0 flex-col">
                  <span className="truncate">
                    {option.actor.displayName || option.actor.handle}
                  </span>
                  <span className="text-muted-foreground truncate text-xs">
                    @{option.actor.handle}
                  </span>
                </span>
              </ComboboxItem>
            ) : null
          }
        </ComboboxList>
      </ComboboxContent>
    </Combobox>
  )
}
