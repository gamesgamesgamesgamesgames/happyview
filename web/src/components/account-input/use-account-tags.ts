"use client"

import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from "react"

import { resolveIdentity } from "@/lib/api"
import type { TypeaheadActor } from "@/lib/typeahead"

export interface AccountTag {
  key: string
  input: string
  status: "resolving" | "resolved" | "error"
  did?: string
  handle?: string | null
  displayName?: string
  avatar?: string
  error?: string
}

export function normalizeEntry(raw: string): string {
  const entry = raw.trim().replace(/^@/, "")
  return entry.startsWith("did:") ? entry : entry.toLowerCase()
}

export function splitEntries(text: string): string[] {
  return text.split(/[\s,]+/).map(normalizeEntry).filter(Boolean)
}

function matches(tag: AccountTag, entry: string) {
  return tag.input === entry || tag.did === entry || tag.handle === entry
}

export function useAccountTags({
  max,
  onChange,
  onBlockedChange,
}: {
  max?: number
  onChange: (dids: string[]) => void
  onBlockedChange: (blocked: boolean) => void
}) {
  const [tags, setTags] = useState<AccountTag[]>([])
  const tagsRef = useRef<AccountTag[]>([])
  const nextKey = useRef(0)
  const callbacks = useRef({ onChange, onBlockedChange })
  useLayoutEffect(() => {
    callbacks.current = { onChange, onBlockedChange }
  })

  const update = useCallback((fn: (current: AccountTag[]) => AccountTag[]) => {
    tagsRef.current = fn(tagsRef.current)
    setTags(tagsRef.current)
  }, [])

  const resolve = useCallback(
    (tag: AccountTag, actor?: TypeaheadActor) => {
      resolveIdentity(tag.input)
        .then((identity) =>
          update((current) => {
            // A handle and a DID for the same account resolve to one DID;
            // keep whichever tag resolved first.
            if (current.some((t) => t.key !== tag.key && t.did === identity.did)) {
              return current.filter((t) => t.key !== tag.key)
            }
            const fromActor = actor?.did === identity.did ? actor : undefined
            return current.map((t) =>
              t.key === tag.key
                ? {
                    ...t,
                    status: "resolved",
                    did: identity.did,
                    handle: identity.handle,
                    displayName: fromActor?.displayName,
                    avatar: fromActor?.avatar,
                  }
                : t,
            )
          }),
        )
        .catch((e: unknown) =>
          update((current) =>
            current.map((t) =>
              t.key === tag.key
                ? {
                    ...t,
                    status: "error",
                    error: e instanceof Error ? e.message : String(e),
                  }
                : t,
            ),
          ),
        )
    },
    [update],
  )

  const add = useCallback(
    (entries: string[], actor?: TypeaheadActor) => {
      const current = tagsRef.current
      const capacity = max === undefined ? Infinity : max - current.length
      const fresh: AccountTag[] = []
      for (const raw of entries) {
        if (fresh.length >= capacity) break
        const entry = normalizeEntry(raw)
        if (!entry) continue
        if (current.some((t) => matches(t, entry))) continue
        if (fresh.some((t) => t.input === entry)) continue
        fresh.push({
          key: `account-${nextKey.current++}`,
          input: entry,
          status: "resolving",
        })
      }
      if (fresh.length === 0) return
      update((tags) => [...tags, ...fresh])
      for (const tag of fresh) resolve(tag, actor)
    },
    [max, resolve, update],
  )

  const remove = useCallback(
    (keys: string[]) => {
      if (keys.length === 0) return
      update((current) => current.filter((t) => !keys.includes(t.key)))
    },
    [update],
  )

  useEffect(() => {
    callbacks.current.onChange(
      tags.flatMap((t) => (t.status === "resolved" && t.did ? [t.did] : [])),
    )
    callbacks.current.onBlockedChange(tags.some((t) => t.status !== "resolved"))
  }, [tags])

  return { tags, add, remove }
}
