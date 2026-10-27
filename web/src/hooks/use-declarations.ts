import { useEffect, useState } from "react";
import { getEditorDeclarations } from "@/lib/api";
import type { DeclarationFile } from "@/types/editor";

/** One request per language per page load, shared by every editor on it.
 * The server answers with an ETag, so a reload revalidates cheaply. */
const requests = new Map<string, Promise<DeclarationFile[]>>();

/** One empty list, so a caller memoising on the result sees no change. */
const NONE: DeclarationFile[] = [];

function load(language: string): Promise<DeclarationFile[]> {
  let request = requests.get(language);
  if (!request) {
    request = getEditorDeclarations(language)
      .then((d) => d.files)
      // An editor without declarations still edits; it only completes less.
      .catch(() => {
        requests.delete(language);
        return [];
      });
    requests.set(language, request);
  }
  return request;
}

/** The declarations the installed interpreter for `language` renders for
 * this instance, or none while they load. */
export function useDeclarations(
  language: string | undefined,
): DeclarationFile[] {
  const [loaded, setLoaded] = useState<{
    language: string;
    files: DeclarationFile[];
  }>();
  useEffect(() => {
    if (!language || language === "json") return;
    let live = true;
    void load(language).then((files) => {
      if (live) setLoaded({ language, files });
    });
    return () => {
      live = false;
    };
  }, [language]);
  return loaded && loaded.language === language ? loaded.files : NONE;
}
