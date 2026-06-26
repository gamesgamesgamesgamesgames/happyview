"use client";

import { useCallback, useEffect, useState } from "react";
import { Trash2, Pencil } from "lucide-react";
import { toast } from "sonner";

import { useCurrentUser } from "@/hooks/use-current-user";
import { toastError } from "@/lib/format";
import {
  getScriptVariables,
  upsertScriptVariable,
  deleteScriptVariable,
} from "@/lib/api";
import type { ScriptVariableSummary } from "@/types/script-variables";
import { SiteHeader } from "@/components/site-header";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from "@/components/ui/alert-dialog";
import { Button } from "@/components/ui/button";
import {
  ResponsiveDialog,
  ResponsiveDialogClose,
  ResponsiveDialogContent,
  ResponsiveDialogDescription,
  ResponsiveDialogFooter,
  ResponsiveDialogHeader,
  ResponsiveDialogTitle,
  ResponsiveDialogTrigger,
} from "@/components/ui/responsive-dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";

export default function EnvVariablesPage() {
  const { hasPermission } = useCurrentUser();
  const [vars, setVars] = useState<ScriptVariableSummary[]>([]);

  const load = useCallback(() => {
    getScriptVariables()
      .then(setVars)
      .catch((e) => toastError("Failed to load variables", e));
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  async function handleDeleteVar(key: string) {
    try {
      await deleteScriptVariable(key);
      toast.success("Variable deleted");
      load();
    } catch (e: unknown) {
      toastError("Failed to delete variable", e);
    }
  }

  return (
    <>
      <SiteHeader title="ENV Variables" />
      <div className="flex flex-1 flex-col gap-4 p-4 md:p-6">
        <div className="flex items-center justify-between">
          <div>
            <h2 className="text-lg font-semibold">Script Variables</h2>
            <p className="text-muted-foreground text-sm">
              Define variables that Lua scripts can access via the{" "}
              <code className="text-xs">env</code> global table.
            </p>
          </div>
          {hasPermission("script-variables:create") && (
            <UpsertVariableDialog onSuccess={load} />
          )}
        </div>

        <div className="overflow-clip rounded-lg border">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Key</TableHead>
                <TableHead>Preview</TableHead>
                <TableHead>Updated</TableHead>
                <TableHead className="w-20 sticky right-0 bg-inherit z-[1]" />
              </TableRow>
            </TableHeader>
            <TableBody>
              {vars.length === 0 && (
                <TableRow>
                  <TableCell
                    colSpan={4}
                    className="text-muted-foreground text-center"
                  >
                    No script variables yet. Variables defined here are accessible to Lua scripts via the env global table.
                  </TableCell>
                </TableRow>
              )}
              {vars.map((v) => (
                <TableRow key={v.key}>
                  <TableCell className="font-mono text-sm">
                    {v.key}
                  </TableCell>
                  <TableCell className="font-mono text-sm text-muted-foreground">
                    {v.preview}
                  </TableCell>
                  <TableCell>
                    {new Date(v.updated_at).toLocaleString()}
                  </TableCell>
                  <TableCell className="w-20 sticky right-0 bg-inherit z-[1]">
                    <div className="flex gap-1">
                      <UpsertVariableDialog
                        onSuccess={load}
                        editKey={v.key}
                      />
                      {hasPermission("script-variables:delete") && (
                        <AlertDialog>
                          <AlertDialogTrigger asChild>
                            <Button
                              variant="destructive"
                              size="icon"
                              className="size-8 text-muted-foreground hover:text-destructive"
                              title="Delete variable"
                              aria-label="Delete variable"
                            >
                              <Trash2 className="size-4" />
                            </Button>
                          </AlertDialogTrigger>
                          <AlertDialogContent>
                            <AlertDialogHeader>
                              <AlertDialogTitle>Delete variable?</AlertDialogTitle>
                              <AlertDialogDescription>
                                This will permanently remove the variable. Scripts
                                using this variable will fail on next execution.
                              </AlertDialogDescription>
                            </AlertDialogHeader>
                            <AlertDialogFooter>
                              <AlertDialogCancel>Cancel</AlertDialogCancel>
                              <AlertDialogAction
                                variant="destructive"
                                onClick={() => handleDeleteVar(v.key)}
                              >
                                Delete
                              </AlertDialogAction>
                            </AlertDialogFooter>
                          </AlertDialogContent>
                        </AlertDialog>
                      )}
                    </div>
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
      </div>
    </>
  );
}

function UpsertVariableDialog({
  onSuccess,
  editKey,
}: {
  onSuccess: () => void;
  editKey?: string;
}) {
  const [key, setKey] = useState(editKey ?? "");
  const [value, setValue] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [open, setOpen] = useState(false);

  const isEdit = !!editKey;

  async function handleSave() {
    setError(null);
    try {
      await upsertScriptVariable({
        key: isEdit ? editKey : key,
        value,
      });
      toast.success(isEdit ? "Variable updated" : "Variable created");
      setKey(editKey ?? "");
      setValue("");
      setOpen(false);
      onSuccess();
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }

  return (
    <ResponsiveDialog
      open={open}
      onOpenChange={(o) => {
        setOpen(o);
        if (o) {
          setKey(editKey ?? "");
          setValue("");
          setError(null);
        }
      }}
    >
      <ResponsiveDialogTrigger asChild>
        {isEdit ? (
          <Button
            variant="ghost"
            size="icon"
            className="size-8"
            title="Edit variable"
            aria-label="Edit variable"
          >
            <Pencil className="size-4" />
          </Button>
        ) : (
          <Button>Add Variable</Button>
        )}
      </ResponsiveDialogTrigger>
      <ResponsiveDialogContent>
        <ResponsiveDialogHeader>
          <ResponsiveDialogTitle>
            {isEdit ? "Edit Variable" : "Add Variable"}
          </ResponsiveDialogTitle>
          <ResponsiveDialogDescription>
            {isEdit
              ? "Update the value for this script variable."
              : "Add a new script variable accessible via env.KEY in Lua scripts."}
          </ResponsiveDialogDescription>
        </ResponsiveDialogHeader>
        <div className="flex flex-col gap-4">
          {error && <p className="text-destructive text-sm">{error}</p>}
          <div className="flex flex-col gap-2">
            <Label htmlFor="var-key">Key</Label>
            <Input
              id="var-key"
              value={key}
              onChange={(e) => setKey(e.target.value)}
              placeholder="VARIABLE_NAME"
              disabled={isEdit}
              className={isEdit ? "font-mono" : ""}
            />
          </div>
          <div className="flex flex-col gap-2">
            <Label htmlFor="var-value">Value</Label>
            <Textarea
              id="var-value"
              value={value}
              onChange={(e) => setValue(e.target.value)}
              placeholder="Enter value..."
              rows={3}
            />
          </div>
        </div>
        <ResponsiveDialogFooter>
          <ResponsiveDialogClose asChild>
            <Button variant="outline">Cancel</Button>
          </ResponsiveDialogClose>
          <Button onClick={handleSave}>Save</Button>
        </ResponsiveDialogFooter>
      </ResponsiveDialogContent>
    </ResponsiveDialog>
  );
}
