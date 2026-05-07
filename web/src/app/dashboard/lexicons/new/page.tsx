"use client";

import { useEffect, useMemo, useRef, useState } from "react";
import { useRouter } from "next/navigation";
import { Empty, EmptyDescription, EmptyTitle } from "@/components/ui/empty";
import { useCurrentUser } from "@/hooks/use-current-user";
import {
  addNetworkLexicon,
  uploadLexicon,
} from "@/lib/api";
import { resolveNsid } from "@/lib/nsid";
import { LEXICON_TEMPLATE } from "@/lib/lua-templates";
import { CodePanels } from "@/components/code-panels";
import { SiteHeader } from "@/components/site-header";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";

export default function AddLexiconPage() {
  const { hasPermission, loading } = useCurrentUser();
  const router = useRouter();
  const [error, setError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  // Redirect if the user cannot create lexicons (only after loading completes)
  useEffect(() => {
    if (!loading && !hasPermission("lexicons:create")) {
      router.replace("/dashboard/lexicons");
    }
  }, [hasPermission, loading, router]);

  // Local state
  const [json, setJson] = useState(LEXICON_TEMPLATE);
  const [localTargetCollection, setLocalTargetCollection] = useState("");
  const [backfill, setBackfill] = useState(true);

  // Network state
  const [nsid, setNsid] = useState("");
  const [networkTargetCollection, setNetworkTargetCollection] = useState("");
  const [networkJson, setNetworkJson] = useState("");
  const [mainType, setMainType] = useState<string | undefined>();
  const [resolving, setResolving] = useState(false);
  const abortRef = useRef<AbortController | null>(null);

  const lastValidType = useRef<string | undefined>(undefined);
  const localMainType = useMemo(() => {
    try {
      const parsed = JSON.parse(json);
      const type = parsed?.defs?.main?.type as string | undefined;
      lastValidType.current = type;
      return type;
    } catch {
      return lastValidType.current;
    }
  }, [json]);

  const showLocalTargetCollection =
    localMainType === "query" || localMainType === "procedure";
  const prevType = useRef(localMainType);
  useEffect(() => {
    if (prevType.current !== localMainType) {
      prevType.current = localMainType;
    }
  }, [localMainType]);

  // Debounced NSID resolution
  useEffect(() => {
    abortRef.current?.abort();
    setMainType(undefined);
    setNetworkJson("");

    if (nsid.split(".").length < 3) return;

    const debounce = setTimeout(() => {
      const controller = new AbortController();
      abortRef.current = controller;
      setResolving(true);

      resolveNsid(nsid, controller.signal)
        .then((result) => {
          if (!controller.signal.aborted) {
            setMainType(result.type);
            setNetworkJson(
              result.lexiconJson
                ? JSON.stringify(result.lexiconJson, null, 2)
                : "",
            );
          }
        })
        .finally(() => {
          if (!controller.signal.aborted) setResolving(false);
        });
    }, 500);

    return () => clearTimeout(debounce);
  }, [nsid]);

  const showNetworkTargetCollection =
    mainType === "query" || mainType === "procedure";

  async function handleUploadLocal() {
    setError(null);
    setSubmitting(true);
    try {
      const lexiconJson = JSON.parse(json);
      await uploadLexicon({
        lexicon_json: lexiconJson,
        backfill: localMainType === "record" && backfill,
      });
      router.push("/dashboard/lexicons");
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e));
      setSubmitting(false);
    }
  }

  async function handleAddNetwork() {
    setError(null);
    setSubmitting(true);
    try {
      await addNetworkLexicon({
        nsid,
        target_collection: showNetworkTargetCollection
          ? networkTargetCollection || undefined
          : undefined,
      });
      router.push("/dashboard/lexicons");
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e));
      setSubmitting(false);
    }
  }

  return (
    <>
      <SiteHeader title="Add Lexicon" backHref="/dashboard/lexicons" />

      <div className="flex flex-1 flex-col">
        <Tabs
          defaultValue="local"
          className="flex flex-col flex-1 gap-0 min-h-0"
        >
          <div className="p-4 md:p-6">
            <TabsList className="w-full max-w-md">
              <TabsTrigger value="local" className="flex-1">
                Local
              </TabsTrigger>
              <TabsTrigger value="network" className="flex-1">
                Network
              </TabsTrigger>
            </TabsList>
          </div>

          <TabsContent value="local" className="flex flex-col flex-1 min-h-0">
            <div className="flex flex-col flex-1 min-h-0 gap-6 p-4 pt-0 md:p-6 md:pt-0">
              {error && <p className="text-destructive text-sm">{error}</p>}

              {/* Metadata fields */}
              {/* <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
                {showLocalTargetCollection && (
                  <div className="flex flex-col gap-2">
                    <Label htmlFor="target-collection">
                      Record Collection (optional)
                    </Label>
                    <Input
                      id="target-collection"
                      value={localTargetCollection}
                      onChange={(e) => setLocalTargetCollection(e.target.value)}
                      placeholder="com.example.record"
                    />
                  </div>
                )}
              </div> */}

              {/* Code panels */}
              <CodePanels
                className="flex-1 min-h-0"
                jsonValue={json}
                onJsonChange={setJson}
              />
            </div>

            <footer className="bg-sidebar-accent flex justify-end gap-6 ps-4 pt-2 pb-1 md:px-6 md:py-4 rounded-b-md">
              {localMainType === "record" && (
                <div className="flex items-center gap-2">
                  <Label htmlFor="backfill">Enable backfill for lexicon</Label>
                  <Switch
                    id="backfill"
                    checked={backfill}
                    onCheckedChange={setBackfill}
                  />
                </div>
              )}

              <Button onClick={handleUploadLocal} disabled={submitting}>
                {submitting ? "Uploading..." : "Upload"}
              </Button>
            </footer>
          </TabsContent>

          <TabsContent value="network" className="flex flex-col flex-1 min-h-0">
            <div className="flex flex-col flex-1 min-h-0 gap-6 p-4 pt-0 md:p-6 md:pt-0">
              {error && <p className="text-destructive text-sm">{error}</p>}

              <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
                <div className="flex flex-col gap-2">
                  <Label htmlFor="nsid">NSID</Label>
                  <Input
                    id="nsid"
                    value={nsid}
                    onChange={(e) => setNsid(e.target.value)}
                    placeholder="com.example.record"
                  />
                </div>

                {showNetworkTargetCollection && (
                  <div className="flex flex-col gap-2">
                    <Label htmlFor="nl-target-collection">
                      Target Collection (optional)
                    </Label>
                    <Input
                      id="nl-target-collection"
                      value={networkTargetCollection}
                      onChange={(e) =>
                        setNetworkTargetCollection(e.target.value)
                      }
                      placeholder="com.example.record"
                    />
                  </div>
                )}
              </div>

              {resolving && (
                <Empty>
                  <EmptyDescription>{"Resolving lexicon..."}</EmptyDescription>
                </Empty>
              )}

              {Boolean(nsid) && !resolving && !networkJson && (
                <Empty>
                  <EmptyTitle>{"Not found"}</EmptyTitle>

                  <EmptyDescription>
                    {"There are no lexicons on the network with NSID:"}
                    <br />
                    <code>{nsid}</code>
                  </EmptyDescription>
                </Empty>
              )}

              {networkJson && (
                <CodePanels
                  className="flex-1 min-h-0"
                  jsonValue={networkJson}
                  jsonReadOnly
                />
              )}
            </div>

            <footer className="bg-sidebar-accent flex justify-end gap-2 ps-4 pt-2 pb-1 md:px-6 md:py-4 rounded-b-md">
              <Button onClick={handleAddNetwork} disabled={submitting}>
                {submitting ? "Adding..." : "Add"}
              </Button>
            </footer>
          </TabsContent>
        </Tabs>
      </div>
    </>
  );
}
