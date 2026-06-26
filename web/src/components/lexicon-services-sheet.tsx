"use client";

import { useCallback, useEffect, useState } from "react";

import { getLexiconServices, removeServiceEntryXrpcs } from "@/lib/api";
import type { ServiceEntry } from "@/lib/api";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
} from "@/components/ui/sheet";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from "@/components/ui/tooltip";

interface LexiconServicesSheetProps {
  lexiconId: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

export function LexiconServicesSheet({
  lexiconId,
  open,
  onOpenChange,
}: LexiconServicesSheetProps) {
  const [services, setServices] = useState<ServiceEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [removing, setRemoving] = useState<number | null>(null);

  const load = useCallback(() => {
    if (!open) return;
    setLoading(true);
    setError(null);
    getLexiconServices(lexiconId)
      .then(setServices)
      .catch((e) => setError(e instanceof Error ? e.message : String(e)))
      .finally(() => setLoading(false));
  }, [lexiconId, open]);

  useEffect(() => {
    load();
  }, [load]);

  async function handleRemove(service: ServiceEntry) {
    setRemoving(service.id);
    setError(null);
    try {
      await removeServiceEntryXrpcs(service.id, [lexiconId]);
      load();
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setRemoving(null);
    }
  }

  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <SheetContent className="flex flex-col overflow-hidden">
        <SheetHeader>
          <SheetTitle>Services</SheetTitle>
          <SheetDescription>
            Services that can access{" "}
            <span className="font-mono">{lexiconId}</span>.
          </SheetDescription>
        </SheetHeader>

        <div className="flex-1 min-h-0 overflow-y-auto px-4">
          {error && (
            <p className="text-destructive text-sm mb-3">{error}</p>
          )}

          {loading ? (
            <p className="text-muted-foreground text-sm">Loading...</p>
          ) : services.length === 0 ? (
            <p className="text-muted-foreground text-sm">
              No services have access to this XRPC.
            </p>
          ) : (
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Service</TableHead>
                  <TableHead>Access</TableHead>
                  <TableHead className="w-20" />
                </TableRow>
              </TableHeader>
              <TableBody>
                {services.map((service) => {
                  const isAllXrpcs = service.access_mode === "all";
                  return (
                    <TableRow key={service.id}>
                      <TableCell className="font-mono text-xs">
                        {service.fragment_id}
                      </TableCell>
                      <TableCell>
                        {isAllXrpcs ? (
                          <Badge className="bg-green-500/15 text-green-700 dark:text-green-400 border-green-500/30 hover:bg-green-500/20">
                            All XRPCs
                          </Badge>
                        ) : (
                          <Badge className="bg-blue-500/15 text-blue-700 dark:text-blue-400 border-blue-500/30 hover:bg-blue-500/20">
                            Explicit
                          </Badge>
                        )}
                      </TableCell>
                      <TableCell className="text-right">
                        {isAllXrpcs ? (
                          <TooltipProvider>
                            <Tooltip>
                              <TooltipTrigger asChild>
                                <span className="text-muted-foreground text-sm select-none cursor-default">
                                  —
                                </span>
                              </TooltipTrigger>
                              <TooltipContent side="left">
                                Change access mode in service config to remove
                                individual XRPCs.
                              </TooltipContent>
                            </Tooltip>
                          </TooltipProvider>
                        ) : (
                          <Button
                            variant="ghost"
                            size="sm"
                            className="text-destructive hover:text-destructive hover:bg-destructive/10 h-7 px-2"
                            disabled={removing === service.id}
                            onClick={() => handleRemove(service)}
                          >
                            {removing === service.id ? "Removing..." : "Remove"}
                          </Button>
                        )}
                      </TableCell>
                    </TableRow>
                  );
                })}
              </TableBody>
            </Table>
          )}
        </div>

        <div className="px-4 pb-4 pt-2 border-t">
          <p className="text-muted-foreground text-xs">
            Services with &ldquo;All XRPCs&rdquo; access can&rsquo;t be removed
            from individual XRPCs. Change their access mode in service config.
          </p>
        </div>
      </SheetContent>
    </Sheet>
  );
}
