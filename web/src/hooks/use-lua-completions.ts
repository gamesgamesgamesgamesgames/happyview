import { useEffect, useMemo, useState } from "react";
import { getLexicon, getLexicons } from "@/lib/api";
import {
  buildCollectionSchemas,
  extractLuaCompletions,
  extractSchemaProperties,
  type CollectionSchemas,
  type LuaCompletions,
} from "@/lib/lua-completions";

/**
 * Shared hook that builds Lua completions from the current JSON text,
 * API-fetched collection schemas, and the live record schema.
 */
export function useLuaCompletions(jsonText: string): {
  luaCompletions: LuaCompletions;
  collections: string[];
} {
  const [collections, setCollections] = useState<string[]>([]);
  const [collectionSchemas, setCollectionSchemas] = useState<CollectionSchemas>(
    {},
  );

  useEffect(() => {
    getLexicons().then(async (lexicons) => {
      const records = lexicons.filter((l) => l.lexicon_type === "record");
      setCollections(records.map((l) => l.id));

      // Fetch individual details to get full lexicon_json for schema extraction
      const details = [];
      for (const rec of records) {
        try {
          details.push(await getLexicon(rec.id));
        } catch {
          // skip failed fetches
        }
      }
      setCollectionSchemas(buildCollectionSchemas(details));
    });
  }, []);

  const luaCompletions = useMemo(() => {
    const completions = extractLuaCompletions(jsonText);

    // Merge collection schemas from the API
    for (const [nsid, entries] of Object.entries(collectionSchemas)) {
      completions[nsid] = entries;
    }

    // Merge live record schema from the current JSON editor
    try {
      const parsed = JSON.parse(jsonText);
      const nsid = parsed?.id;
      const mainDef = parsed?.defs?.main;
      if (nsid && mainDef?.type === "record") {
        const props = extractSchemaProperties(mainDef.record);
        if (props.length) completions[nsid] = props;
      }
    } catch {
      // invalid JSON — keep existing completions
    }

    return completions;
  }, [jsonText, collectionSchemas]);

  return { luaCompletions, collections };
}
