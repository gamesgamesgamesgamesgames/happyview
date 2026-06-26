"use client";

import { useCallback, useEffect, useState } from "react";
import {
  AlertTriangle,
  Copy,
  Check,
  Trash2,
  X,
  ExternalLink,
} from "lucide-react";
import { toast } from "sonner";

import { useConfig } from "@/lib/config-context";
import { useCurrentUser } from "@/hooks/use-current-user";
import { toastError } from "@/lib/format";
import {
  getApiClients,
  createApiClient,
  updateApiClient,
  deleteApiClient,
} from "@/lib/api";
import type {
  ApiClientSummary,
  CreateApiClientResponse,
} from "@/types/api-clients";
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
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import { Switch } from "@/components/ui/switch";
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
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";

function MultiInput({
  values,
  onChange,
  placeholder,
  readonlyValues = [],
  id,
}: {
  values: string[];
  onChange: (values: string[]) => void;
  placeholder?: string;
  readonlyValues?: string[];
  id?: string;
}) {
  function handleChange(index: number, value: string) {
    const next = [...values];
    next[index] = value;
    // If user typed into the last input, add an empty one
    if (index === values.length - 1 && value.trim() !== "") {
      next.push("");
    }
    onChange(next);
  }

  function handleRemove(index: number) {
    const next = values.filter((_, i) => i !== index);
    // Always keep at least one empty input
    if (next.length === 0 || next[next.length - 1].trim() !== "") {
      next.push("");
    }
    onChange(next);
  }

  function handleKeyDown(
    index: number,
    e: React.KeyboardEvent<HTMLInputElement>,
  ) {
    if (e.key === "Backspace" && values[index] === "" && values.length > 1) {
      e.preventDefault();
      handleRemove(index);
    }
  }

  return (
    <div className="flex flex-col gap-1.5">
      {readonlyValues.map((val, i) => (
        <Input
          key={`readonly-${i}`}
          value={val}
          readOnly
          className="font-mono text-sm bg-muted"
        />
      ))}
      {values.map((val, index) => (
        <div key={index} className="flex gap-1.5">
          <Input
            id={index === 0 ? id : undefined}
            value={val}
            onChange={(e) => handleChange(index, e.target.value)}
            onKeyDown={(e) => handleKeyDown(index, e)}
            placeholder={placeholder}
            className="font-mono text-sm"
          />
          {values.length > 1 && val.trim() !== "" && (
            <Button
              type="button"
              variant="ghost"
              size="icon"
              className="shrink-0 size-9 text-muted-foreground hover:text-destructive"
              onClick={() => handleRemove(index)}
            >
              <X className="size-4" />
            </Button>
          )}
        </div>
      ))}
    </div>
  );
}

export default function ApiClientsPage() {
  const { hasPermission } = useCurrentUser();
  const [clients, setClients] = useState<ApiClientSummary[]>([]);

  const load = useCallback(() => {
    getApiClients()
      .then(setClients)
      .catch((e) => toastError("Failed to load API clients", e));
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  return (
    <>
      <SiteHeader title="API Clients" />
      <div className="flex flex-1 flex-col gap-4 p-4 md:p-6">
        <div className="flex items-center justify-between">
          <div>
            <h2 className="text-lg font-semibold">API Clients</h2>
            <p className="text-muted-foreground text-sm">
              Registered applications that authenticate through this AppView.
            </p>
          </div>
          {hasPermission("api-clients:create") && (
            <CreateApiClientDialog onSuccess={load} />
          )}
        </div>

        <div className="overflow-clip rounded-lg border">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Name</TableHead>
                <TableHead>Type</TableHead>
                <TableHead>Client Key</TableHead>
                <TableHead>Client ID URL</TableHead>
                <TableHead>Scopes</TableHead>
                <TableHead>Status</TableHead>
                <TableHead>Created</TableHead>
                <TableHead>Parent Client</TableHead>
                <TableHead>Owner</TableHead>
                <TableHead className="w-10 sticky right-0 bg-inherit z-[1]" />
              </TableRow>
            </TableHeader>
            <TableBody>
              {clients.length === 0 && (
                <TableRow>
                  <TableCell
                    colSpan={10}
                    className="text-muted-foreground text-center"
                  >
                    No API clients yet. Register an application to enable OAuth authentication through this AppView.
                  </TableCell>
                </TableRow>
              )}
              {clients.map((client) => (
                <TableRow
                  key={client.id}
                  className={!client.is_active ? "opacity-50" : undefined}
                >
                  <TableCell className="font-medium">{client.name}</TableCell>
                  <TableCell>
                    <Badge variant="outline">
                      {client.client_type === "public"
                        ? "Public"
                        : "Confidential"}
                    </Badge>
                  </TableCell>
                  <TableCell className="font-mono text-sm">
                    {client.client_key}
                  </TableCell>
                  <TableCell className="max-w-48 truncate text-sm">
                    {client.client_id_url}
                  </TableCell>
                  <TableCell>
                    <div className="flex flex-wrap gap-1">
                      {client.scopes
                        .split(/\s+/)
                        .filter(Boolean)
                        .map((scope) => (
                          <Badge key={scope} variant="secondary">
                            {scope}
                          </Badge>
                        ))}
                    </div>
                  </TableCell>
                  <TableCell>
                    <Badge variant={client.is_active ? "default" : "outline"}>
                      {client.is_active ? "Active" : "Inactive"}
                    </Badge>
                  </TableCell>
                  <TableCell>
                    {new Date(client.created_at).toLocaleString()}
                  </TableCell>
                  <TableCell className="text-sm">
                    {client.parent_client_id
                      ? (clients.find((c) => c.id === client.parent_client_id)
                          ?.name ?? client.parent_client_id)
                      : "—"}
                  </TableCell>
                  <TableCell className="text-sm max-w-48 truncate">
                    {client.owner_did ?? "—"}
                  </TableCell>
                  <TableCell className="w-10 sticky right-0 bg-inherit z-[1]">
                    <div className="flex gap-1">
                      {hasPermission("api-clients:edit") && (
                        <EditApiClientDialog client={client} onSuccess={load} />
                      )}
                      {hasPermission("api-clients:delete") && (
                        <DeleteApiClientDialog
                          client={client}
                          onSuccess={load}
                        />
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

function CreateApiClientDialog({ onSuccess }: { onSuccess: () => void }) {
  const config = useConfig();

  const [clientType, setClientType] = useState<"confidential" | "public">(
    "confidential",
  );
  const [name, setName] = useState("");
  const [clientIdUrl, setClientIdUrl] = useState("");
  const [clientUri, setClientUri] = useState("");
  const [redirectUris, setRedirectUris] = useState<string[]>([""]);
  const [allowedOrigins, setAllowedOrigins] = useState<string[]>([""]);
  const [scopes, setScopes] = useState<string[]>([""]);
  const [rateLimitEnabled, setRateLimitEnabled] = useState(true);
  const [rateLimitCapacity, setRateLimitCapacity] = useState(
    String(config.default_rate_limit_capacity),
  );
  const [rateLimitRefillRate, setRateLimitRefillRate] = useState(
    String(config.default_rate_limit_refill_rate),
  );
  const [error, setError] = useState<string | null>(null);
  const [open, setOpen] = useState(false);
  const [created, setCreated] = useState<CreateApiClientResponse | null>(null);
  const [copiedField, setCopiedField] = useState<string | null>(null);

  function handleOpenChange(nextOpen: boolean) {
    setOpen(nextOpen);
    if (!nextOpen) {
      setClientType("confidential");
      setName("");
      setClientIdUrl("");
      setClientUri("");
      setRedirectUris([""]);
      setAllowedOrigins([""]);
      setScopes([""]);
      setRateLimitEnabled(true);
      setRateLimitCapacity(String(config.default_rate_limit_capacity));
      setRateLimitRefillRate(String(config.default_rate_limit_refill_rate));
      setError(null);
      if (created) {
        setCreated(null);
        onSuccess();
      }
    }
  }

  async function handleCopy(value: string, field: string) {
    await navigator.clipboard.writeText(value);
    setCopiedField(field);
    setTimeout(() => setCopiedField(null), 2000);
  }

  async function handleCreate() {
    setError(null);
    const allUris = redirectUris.map((u) => u.trim()).filter(Boolean);
    const extraScopes = scopes.map((s) => s.trim()).filter(Boolean);
    const allScopes = ["atproto", ...extraScopes].join(" ");

    if (!name.trim() || !clientIdUrl.trim() || !clientUri.trim()) {
      setError("Name, Client ID URL, and Client URI are required.");
      return;
    }
    if (rateLimitEnabled && (!rateLimitCapacity || !rateLimitRefillRate)) {
      setError("Rate limit capacity and refill rate are required.");
      return;
    }
    try {
      const filteredOrigins = allowedOrigins
        .map((o) => o.trim())
        .filter(Boolean);
      const result = await createApiClient({
        name: name.trim(),
        client_id_url: clientIdUrl.trim(),
        client_uri: clientUri.trim(),
        redirect_uris: allUris,
        scopes: allScopes,
        client_type: clientType,
        allowed_origins:
          clientType === "public" && filteredOrigins.length > 0
            ? filteredOrigins
            : undefined,
        rate_limit_capacity: rateLimitEnabled
          ? Number(rateLimitCapacity)
          : null,
        rate_limit_refill_rate: rateLimitEnabled
          ? Number(rateLimitRefillRate)
          : null,
      });
      setCreated(result);
    } catch (e: unknown) {
      toastError("Failed to create API client", e);
    }
  }

  return (
    <ResponsiveDialog open={open} onOpenChange={handleOpenChange}>
      <ResponsiveDialogTrigger asChild>
        <Button>Create API Client</Button>
      </ResponsiveDialogTrigger>
      <ResponsiveDialogContent>
        <ResponsiveDialogHeader>
          <ResponsiveDialogTitle>
            {created ? "API Client Created" : "Create API Client"}
          </ResponsiveDialogTitle>
          <ResponsiveDialogDescription>
            {created
              ? created.client_type === "public"
                ? "Your public client has been created. Use PKCE for authentication."
                : "Save the credentials below. The secret will not be shown again."
              : "Register a new application that authenticates through this AppView."}
          </ResponsiveDialogDescription>
        </ResponsiveDialogHeader>

        {created ? (
          <div className="flex flex-col gap-4">
            <div className="flex flex-col gap-2">
              <Label>Client Key</Label>
              <div className="flex gap-2">
                <Input
                  readOnly
                  value={created.client_key}
                  className="font-mono text-sm"
                />
                <Button
                  variant="outline"
                  size="icon"
                  onClick={() => handleCopy(created.client_key, "key")}
                  title="Copy to clipboard"
                >
                  {copiedField === "key" ? (
                    <Check className="size-4" />
                  ) : (
                    <Copy className="size-4" />
                  )}
                </Button>
              </div>
              <p className="text-muted-foreground text-xs">
                Public identifier. Send as the{" "}
                <code className="bg-muted px-1 rounded">X-Client-Key</code>{" "}
                header or{" "}
                <code className="bg-muted px-1 rounded">client_key</code> query
                parameter.
              </p>
            </div>
            {created.client_secret ? (
              <div className="flex flex-col gap-2">
                <Label>Client Secret</Label>
                <div className="flex gap-2">
                  <Input
                    readOnly
                    value={created.client_secret}
                    className="font-mono text-sm"
                  />
                  <Button
                    variant="outline"
                    size="icon"
                    onClick={() => handleCopy(created.client_secret!, "secret")}
                    title="Copy to clipboard"
                  >
                    {copiedField === "secret" ? (
                      <Check className="size-4" />
                    ) : (
                      <Copy className="size-4" />
                    )}
                  </Button>
                </div>
                <p className="text-muted-foreground text-xs">
                  Keep this secret. Send as the{" "}
                  <code className="bg-muted px-1 rounded">X-Client-Secret</code>{" "}
                  header for server-to-server requests. Browser requests are
                  validated by Origin instead.
                </p>
              </div>
            ) : (
              <div className="flex flex-col gap-2 rounded-lg border p-4 bg-muted/50">
                <p className="text-sm">
                  This is a public client. Authenticate using PKCE instead of a
                  client secret.
                </p>
                <a
                  href="/docs/getting-started/authentication#pkce"
                  target="_blank"
                  rel="noopener noreferrer"
                  className="inline-flex items-center gap-1 text-sm text-primary hover:underline"
                >
                  PKCE authentication docs
                  <ExternalLink className="size-3" />
                </a>
              </div>
            )}
          </div>
        ) : (
          <div className="flex flex-col gap-4 max-h-[60vh] overflow-y-auto">
            {error && <p className="text-destructive text-sm">{error}</p>}
            <fieldset className="flex flex-col gap-3 rounded-lg border p-4">
              <legend className="text-sm font-medium px-1">Client Type</legend>
              <RadioGroup
                value={clientType}
                onValueChange={(v) =>
                  setClientType(v as "confidential" | "public")
                }
                className="flex flex-col gap-3"
              >
                <div className="flex items-start gap-3">
                  <RadioGroupItem
                    value="confidential"
                    id="type-confidential"
                    className="mt-0.5"
                  />
                  <div className="flex flex-col gap-0.5">
                    <Label
                      htmlFor="type-confidential"
                      className="cursor-pointer font-medium"
                    >
                      Confidential
                    </Label>
                    <p className="text-muted-foreground text-xs">
                      Server-side applications that can securely store a client
                      secret.
                    </p>
                  </div>
                </div>
                <div className="flex items-start gap-3">
                  <RadioGroupItem
                    value="public"
                    id="type-public"
                    className="mt-0.5"
                  />
                  <div className="flex flex-col gap-0.5">
                    <Label
                      htmlFor="type-public"
                      className="cursor-pointer font-medium"
                    >
                      Public
                    </Label>
                    <p className="text-muted-foreground text-xs">
                      Browser or native apps that authenticate using PKCE (no
                      secret).
                    </p>
                  </div>
                </div>
              </RadioGroup>
            </fieldset>
            <fieldset className="flex flex-col gap-3 rounded-lg border p-4">
              <legend className="text-sm font-medium px-1">Application</legend>
              <div className="flex flex-col gap-2">
                <Label htmlFor="client-name">Name</Label>
                <Input
                  id="client-name"
                  value={name}
                  onChange={(e) => setName(e.target.value)}
                  placeholder="My App"
                />
              </div>
              <div className="flex flex-col gap-2">
                <Label htmlFor="client-id-url">Client ID URL</Label>
                <Input
                  id="client-id-url"
                  value={clientIdUrl}
                  onChange={(e) => setClientIdUrl(e.target.value)}
                  placeholder="https://example.com/oauth-client-metadata.json"
                  className="font-mono text-sm"
                />
                <p className="text-muted-foreground text-xs">
                  The URL where the client metadata JSON is served.
                </p>
              </div>
              <div className="flex flex-col gap-2">
                <Label htmlFor="client-uri">Client URI</Label>
                <Input
                  id="client-uri"
                  value={clientUri}
                  onChange={(e) => setClientUri(e.target.value)}
                  placeholder="https://example.com"
                  className="font-mono text-sm"
                />
              </div>
            </fieldset>
            <fieldset className="flex flex-col gap-3 rounded-lg border p-4">
              <legend className="text-sm font-medium px-1">
                Redirect URIs
              </legend>
              <p className="text-muted-foreground text-xs">
                URLs that the authorization server may redirect to after
                authentication. The AppView callback is always included.
              </p>
              <MultiInput
                id="redirect-uris"
                values={redirectUris}
                onChange={setRedirectUris}
                placeholder="https://example.com/auth/callback"
              />
            </fieldset>
            {clientType === "public" && (
              <fieldset className="flex flex-col gap-3 rounded-lg border p-4">
                <legend className="text-sm font-medium px-1">
                  Allowed Origins
                </legend>
                <p className="text-muted-foreground text-xs">
                  Origins permitted to use this client. Requests from unlisted
                  origins will be rejected. Leave empty to allow any origin.
                </p>
                <MultiInput
                  id="allowed-origins"
                  values={allowedOrigins}
                  onChange={setAllowedOrigins}
                  placeholder="https://myapp.com"
                />
              </fieldset>
            )}
            <fieldset className="flex flex-col gap-3 rounded-lg border p-4">
              <legend className="text-sm font-medium px-1">Scopes</legend>
              <p className="text-muted-foreground text-xs">
                OAuth scopes this client is allowed to request. The{" "}
                <code className="bg-muted px-1 rounded">atproto</code> scope is
                always required.
              </p>
              <MultiInput
                id="scopes"
                values={scopes}
                onChange={setScopes}
                placeholder="scope.name"
                readonlyValues={["atproto"]}
              />
              {scopes.some((s) => s.trim() === "transition:generic") && (
                <div className="flex items-start gap-3 rounded-lg border border-amber-500/50 bg-amber-500/10 p-3">
                  <AlertTriangle className="size-4 text-amber-500 shrink-0 mt-0.5" />
                  <p className="text-xs text-amber-500">
                    <code>transition:generic</code> grants broad write access to
                    any collection. Prefer specific scopes or{" "}
                    <a
                      href="https://docs.happyview.dev/guides/features/api-clients#permission-sets"
                      target="_blank"
                      rel="noopener noreferrer"
                      className="underline"
                    >
                      permission sets
                    </a>
                    .
                  </p>
                </div>
              )}
            </fieldset>
            <fieldset className="flex flex-col gap-3 rounded-lg border p-4">
              <legend className="text-sm font-medium px-1">
                Rate Limiting
              </legend>
              <div className="flex items-center gap-3">
                <Switch
                  id="rl-enabled"
                  checked={rateLimitEnabled}
                  onCheckedChange={setRateLimitEnabled}
                />
                <Label htmlFor="rl-enabled" className="cursor-pointer">
                  Enabled
                </Label>
              </div>
              <p className="text-muted-foreground text-xs">
                Each client gets a token bucket. Requests consume tokens and the
                bucket refills over time. When the bucket is empty, requests are
                rejected until tokens replenish.
              </p>
              <div className="grid grid-cols-2 gap-4">
                <div className="flex flex-col gap-2">
                  <Label htmlFor="rl-capacity">Bucket Size</Label>
                  <Input
                    id="rl-capacity"
                    type="number"
                    min={1}
                    value={rateLimitCapacity}
                    onChange={(e) => setRateLimitCapacity(e.target.value)}
                    disabled={!rateLimitEnabled}
                  />
                  <p className="text-muted-foreground text-xs">
                    Maximum number of tokens. This is the burst limit.
                  </p>
                </div>
                <div className="flex flex-col gap-2">
                  <Label htmlFor="rl-refill">Refill Rate</Label>
                  <Input
                    id="rl-refill"
                    type="number"
                    min={0.01}
                    step="any"
                    value={rateLimitRefillRate}
                    onChange={(e) => setRateLimitRefillRate(e.target.value)}
                    disabled={!rateLimitEnabled}
                  />
                  <p className="text-muted-foreground text-xs">
                    Tokens added per second.
                  </p>
                </div>
              </div>
            </fieldset>
          </div>
        )}

        <ResponsiveDialogFooter>
          <ResponsiveDialogClose asChild>
            <Button variant={created ? "default" : "outline"}>
              {created ? "Done" : "Cancel"}
            </Button>
          </ResponsiveDialogClose>
          {!created && (
            <Button onClick={handleCreate} disabled={!name.trim()}>
              Create
            </Button>
          )}
        </ResponsiveDialogFooter>
      </ResponsiveDialogContent>
    </ResponsiveDialog>
  );
}

function EditApiClientDialog({
  client,
  onSuccess,
}: {
  client: ApiClientSummary;
  onSuccess: () => void;
}) {
  const config = useConfig();

  function parseRedirectUris(uris: string[]): string[] {
    return uris.length > 0 ? [...uris, ""] : [""];
  }

  // Parse existing scopes: separate "atproto" from user-added ones
  function parseScopes(scopeStr: string): string[] {
    const parts = scopeStr.split(/\s+/).filter((s) => s && s !== "atproto");
    return parts.length > 0 ? [...parts, ""] : [""];
  }

  function parseAllowedOrigins(origins: string[] | null): string[] {
    if (!origins || origins.length === 0) return [""];
    return [...origins, ""];
  }

  const [name, setName] = useState(client.name);
  const [redirectUris, setRedirectUris] = useState<string[]>(
    parseRedirectUris(client.redirect_uris),
  );
  const [allowedOrigins, setAllowedOrigins] = useState<string[]>(
    parseAllowedOrigins(client.allowed_origins),
  );
  const [scopes, setScopes] = useState<string[]>(parseScopes(client.scopes));
  const [isActive, setIsActive] = useState(client.is_active);
  const [rateLimitEnabled, setRateLimitEnabled] = useState(
    client.rate_limit_capacity != null && client.rate_limit_refill_rate != null,
  );
  const [rateLimitCapacity, setRateLimitCapacity] = useState(
    String(client.rate_limit_capacity ?? config.default_rate_limit_capacity),
  );
  const [rateLimitRefillRate, setRateLimitRefillRate] = useState(
    String(
      client.rate_limit_refill_rate ?? config.default_rate_limit_refill_rate,
    ),
  );
  const [error, setError] = useState<string | null>(null);
  const [open, setOpen] = useState(false);
  const [saving, setSaving] = useState(false);

  function handleOpenChange(nextOpen: boolean) {
    setOpen(nextOpen);
    if (nextOpen) {
      setName(client.name);
      setRedirectUris(parseRedirectUris(client.redirect_uris));
      setAllowedOrigins(parseAllowedOrigins(client.allowed_origins));
      setScopes(parseScopes(client.scopes));
      setIsActive(client.is_active);
      setRateLimitEnabled(
        client.rate_limit_capacity != null &&
          client.rate_limit_refill_rate != null,
      );
      setRateLimitCapacity(
        String(
          client.rate_limit_capacity ?? config.default_rate_limit_capacity,
        ),
      );
      setRateLimitRefillRate(
        String(
          client.rate_limit_refill_rate ??
            config.default_rate_limit_refill_rate,
        ),
      );
      setError(null);
    }
  }

  async function handleSave() {
    setError(null);
    if (rateLimitEnabled && (!rateLimitCapacity || !rateLimitRefillRate)) {
      setError("Rate limit capacity and refill rate are required.");
      return;
    }
    setSaving(true);
    try {
      const allUris = redirectUris.map((u) => u.trim()).filter(Boolean);
      const extraScopes = scopes.map((s) => s.trim()).filter(Boolean);
      const allScopes = ["atproto", ...extraScopes].join(" ");

      const filteredOrigins = allowedOrigins
        .map((o) => o.trim())
        .filter(Boolean);
      await updateApiClient(client.id, {
        name: name.trim() || undefined,
        redirect_uris: allUris,
        scopes: allScopes,
        allowed_origins:
          client.client_type === "public"
            ? filteredOrigins.length > 0
              ? filteredOrigins
              : null
            : undefined,
        is_active: isActive,
        rate_limit_capacity: rateLimitEnabled
          ? Number(rateLimitCapacity)
          : null,
        rate_limit_refill_rate: rateLimitEnabled
          ? Number(rateLimitRefillRate)
          : null,
      });
      toast.success("API client updated");
      setOpen(false);
      onSuccess();
    } catch (e: unknown) {
      toastError("Failed to update API client", e);
    } finally {
      setSaving(false);
    }
  }

  return (
    <ResponsiveDialog open={open} onOpenChange={handleOpenChange}>
      <ResponsiveDialogTrigger asChild>
        <Button variant="outline" size="sm">
          Edit
        </Button>
      </ResponsiveDialogTrigger>
      <ResponsiveDialogContent>
        <ResponsiveDialogHeader>
          <ResponsiveDialogTitle>Edit API Client</ResponsiveDialogTitle>
          <ResponsiveDialogDescription>
            Update settings for &ldquo;{client.name}&rdquo;.
          </ResponsiveDialogDescription>
        </ResponsiveDialogHeader>
        <div className="flex flex-col gap-4 max-h-[60vh] overflow-y-auto">
          {error && <p className="text-destructive text-sm">{error}</p>}
          <fieldset className="flex flex-col gap-3 rounded-lg border p-4">
            <legend className="text-sm font-medium px-1">Application</legend>
            <div className="flex flex-col gap-2">
              <Label htmlFor="edit-name">Name</Label>
              <Input
                id="edit-name"
                value={name}
                onChange={(e) => setName(e.target.value)}
              />
            </div>
            <div className="flex items-center gap-3">
              <Switch
                id="edit-active"
                checked={isActive}
                onCheckedChange={setIsActive}
              />
              <Label htmlFor="edit-active" className="cursor-pointer">
                Active
              </Label>
            </div>
          </fieldset>
          <fieldset className="flex flex-col gap-3 rounded-lg border p-4">
            <legend className="text-sm font-medium px-1">Redirect URIs</legend>
            <p className="text-muted-foreground text-xs">
              URLs that the authorization server may redirect to after
              authentication. The AppView callback is always included.
            </p>
            <MultiInput
              id="edit-redirect-uris"
              values={redirectUris}
              onChange={setRedirectUris}
              placeholder="https://example.com/auth/callback"
            />
          </fieldset>
          {client.client_type === "public" && (
            <fieldset className="flex flex-col gap-3 rounded-lg border p-4">
              <legend className="text-sm font-medium px-1">
                Allowed Origins
              </legend>
              <p className="text-muted-foreground text-xs">
                Origins permitted to use this client. Requests from unlisted
                origins will be rejected. Leave empty to allow any origin.
              </p>
              <MultiInput
                id="edit-allowed-origins"
                values={allowedOrigins}
                onChange={setAllowedOrigins}
                placeholder="https://myapp.com"
              />
            </fieldset>
          )}
          <fieldset className="flex flex-col gap-3 rounded-lg border p-4">
            <legend className="text-sm font-medium px-1">Scopes</legend>
            <p className="text-muted-foreground text-xs">
              OAuth scopes this client is allowed to request. The{" "}
              <code className="bg-muted px-1 rounded">atproto</code> scope is
              always required.
            </p>
            <MultiInput
              id="edit-scopes"
              values={scopes}
              onChange={setScopes}
              placeholder="scope.name"
              readonlyValues={["atproto"]}
            />
            {scopes.some((s) => s.trim() === "transition:generic") && (
              <div className="flex items-start gap-3 rounded-lg border border-amber-500/50 bg-amber-500/10 p-3">
                <AlertTriangle className="size-4 text-amber-500 shrink-0 mt-0.5" />
                <p className="text-xs text-amber-500">
                  <span className="font-medium">transition:generic</span> grants
                  broad write access to any collection. Prefer specific scopes
                  or{" "}
                  <a
                    href="https://docs.happyview.dev/guides/features/api-clients#permission-sets"
                    target="_blank"
                    rel="noopener noreferrer"
                    className="underline"
                  >
                    permission sets
                  </a>{" "}
                  to follow the principle of least privilege.
                </p>
              </div>
            )}
          </fieldset>
          <fieldset className="flex flex-col gap-3 rounded-lg border p-4">
            <legend className="text-sm font-medium px-1">Rate Limiting</legend>
            <div className="flex items-center gap-3">
              <Switch
                id="edit-rl-enabled"
                checked={rateLimitEnabled}
                onCheckedChange={setRateLimitEnabled}
              />
              <Label htmlFor="edit-rl-enabled" className="cursor-pointer">
                Enabled
              </Label>
            </div>
            <p className="text-muted-foreground text-xs">
              Each client gets a token bucket. Requests consume tokens and the
              bucket refills over time. When the bucket is empty, requests are
              rejected until tokens replenish.
            </p>
            <div className="grid grid-cols-2 gap-4">
              <div className="flex flex-col gap-2">
                <Label htmlFor="edit-rl-capacity">Bucket Size</Label>
                <Input
                  id="edit-rl-capacity"
                  type="number"
                  min={1}
                  value={rateLimitCapacity}
                  onChange={(e) => setRateLimitCapacity(e.target.value)}
                  disabled={!rateLimitEnabled}
                />
                <p className="text-muted-foreground text-xs">
                  Maximum number of tokens. This is the burst limit.
                </p>
              </div>
              <div className="flex flex-col gap-2">
                <Label htmlFor="edit-rl-refill">Refill Rate</Label>
                <Input
                  id="edit-rl-refill"
                  type="number"
                  min={0.01}
                  step="any"
                  value={rateLimitRefillRate}
                  onChange={(e) => setRateLimitRefillRate(e.target.value)}
                  disabled={!rateLimitEnabled}
                />
                <p className="text-muted-foreground text-xs">
                  Tokens added per second.
                </p>
              </div>
            </div>
          </fieldset>
        </div>
        <ResponsiveDialogFooter>
          <ResponsiveDialogClose asChild>
            <Button variant="outline">Cancel</Button>
          </ResponsiveDialogClose>
          <Button onClick={handleSave} disabled={saving}>
            {saving ? "Saving..." : "Save"}
          </Button>
        </ResponsiveDialogFooter>
      </ResponsiveDialogContent>
    </ResponsiveDialog>
  );
}

function DeleteApiClientDialog({
  client,
  onSuccess,
}: {
  client: ApiClientSummary;
  onSuccess: () => void;
}) {
  const [open, setOpen] = useState(false);
  const [deleting, setDeleting] = useState(false);

  async function handleConfirm() {
    setDeleting(true);
    try {
      await deleteApiClient(client.id);
      toast.success("API client deleted");
      setOpen(false);
      onSuccess();
    } catch (e: unknown) {
      toastError("Failed to delete API client", e);
    } finally {
      setDeleting(false);
    }
  }

  return (
    <AlertDialog open={open} onOpenChange={setOpen}>
      <AlertDialogTrigger asChild>
        <Button
          variant="ghost"
          size="icon"
          className="size-8 text-muted-foreground hover:text-destructive"
          title="Delete"
          aria-label="Delete"
        >
          <Trash2 className="size-4" />
        </Button>
      </AlertDialogTrigger>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>Delete API client?</AlertDialogTitle>
          <AlertDialogDescription>
            This will permanently delete &ldquo;{client.name}&rdquo; and revoke
            its OAuth identity. Any applications using this client will lose the
            ability to authenticate.
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel disabled={deleting}>Cancel</AlertDialogCancel>
          <AlertDialogAction
            variant="destructive"
            disabled={deleting}
            onClick={handleConfirm}
          >
            {deleting ? "Deleting..." : "Delete"}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}
