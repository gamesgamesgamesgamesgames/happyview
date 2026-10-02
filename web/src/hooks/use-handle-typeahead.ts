import { useEffect, useRef, useState } from "react";

export interface TypeaheadActor {
  did: string;
  handle: string;
  displayName?: string;
  avatar?: string;
}

export function useHandleTypeahead(query: string, delay = 200) {
  const [fetchedActors, setFetchedActors] = useState<TypeaheadActor[]>([]);
  const abortRef = useRef<AbortController | null>(null);

  useEffect(() => {
    if (query.length < 2) {
      return;
    }

    abortRef.current?.abort();

    const timeout = setTimeout(() => {
      const controller = new AbortController();
      abortRef.current = controller;

      fetch(
        `https://typeahead.waow.tech/xrpc/app.bsky.actor.searchActorsTypeahead?q=${encodeURIComponent(query)}&limit=8`,
        {
          signal: controller.signal,
          headers: { "X-Client": "happyview" },
        },
      )
        .then((res) => (res.ok ? res.json() : Promise.reject()))
        .then((data) => setFetchedActors(data.actors ?? []))
        .catch((e) => {
          if (!(e instanceof DOMException && e.name === "AbortError")) {
            setFetchedActors([]);
          }
        });
    }, delay);

    return () => {
      clearTimeout(timeout);
      abortRef.current?.abort();
    };
  }, [query, delay]);

  const actors = query.length < 2 ? [] : fetchedActors;

  return { actors };
}
