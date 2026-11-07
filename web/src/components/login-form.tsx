"use client";

import { useState } from "react";
import { XIcon } from "lucide-react";

import { cn } from "@/lib/utils";
import { useAuth } from "@/lib/auth-context";
import {
  useHandleTypeahead,
  type TypeaheadActor,
} from "@/hooks/use-handle-typeahead";
import { Avatar, AvatarFallback, AvatarImage } from "@/components/ui/avatar";
import { Button } from "@/components/ui/button";
import {
  Combobox,
  ComboboxContent,
  ComboboxInput,
  ComboboxItem,
  ComboboxList,
} from "@/components/ui/combobox";
import {
  Field,
  FieldDescription,
  FieldGroup,
  FieldLabel,
} from "@/components/ui/field";

export function LoginForm({
  className,
  externalError,
  ...props
}: React.ComponentProps<"div"> & { externalError?: string | null }) {
  const [handle, setHandle] = useState("");
  const [selectedActor, setSelectedActor] = useState<TypeaheadActor | null>(
    null,
  );
  const [dropdownOpen, setDropdownOpen] = useState(false);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const { login } = useAuth();
  const { actors } = useHandleTypeahead(selectedActor ? "" : handle);

  const actorsByHandle = new Map(actors.map((a) => [a.handle, a]));
  const showDropdown = dropdownOpen && actors.length > 0;

  async function handleSubmit(e: React.FormEvent) {
    e.preventDefault();
    const value = selectedActor?.handle ?? handle;
    if (!value.trim()) return;
    setLoading(true);
    setError(null);
    try {
      await login(value.trim().toLowerCase());
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : "Login failed");
      setLoading(false);
    }
  }

  function clearSelection() {
    setSelectedActor(null);
    setHandle("");
  }

  return (
    <div className={cn("flex flex-col gap-6", className)} {...props}>
      <form onSubmit={handleSubmit}>
        <FieldGroup>
          <div className="flex flex-col items-center gap-2 text-center">
            <h1 className="text-xl font-bold">HappyView Admin</h1>
            <FieldDescription>
              Sign in with your ATProto account to manage your AppView.
            </FieldDescription>
          </div>
          {(externalError || error) && (
            <p className="text-destructive text-center text-sm">
              {externalError || error}
            </p>
          )}
          <Field>
            <FieldLabel htmlFor="handle">Handle</FieldLabel>
            {selectedActor ? (
              <div className="border-input dark:bg-input/30 flex w-full items-center gap-3 rounded-md border px-3 py-2 shadow-xs">
                <Avatar size="sm">
                  {selectedActor.avatar && (
                    <AvatarImage
                      src={selectedActor.avatar}
                      alt={selectedActor.handle}
                    />
                  )}
                  <AvatarFallback>
                    {(
                      selectedActor.displayName?.[0] ?? selectedActor.handle[0]
                    ).toUpperCase()}
                  </AvatarFallback>
                </Avatar>
                <div className="flex min-w-0 flex-1 flex-col">
                  {selectedActor.displayName && (
                    <span className="truncate text-sm font-medium">
                      {selectedActor.displayName}
                    </span>
                  )}
                  <span className="text-muted-foreground truncate text-xs">
                    @{selectedActor.handle}
                  </span>
                </div>
                <button
                  type="button"
                  onClick={clearSelection}
                  disabled={loading}
                  className="text-muted-foreground hover:text-foreground shrink-0 cursor-pointer disabled:pointer-events-none disabled:opacity-50"
                >
                  <XIcon className="size-4" />
                </button>
              </div>
            ) : (
              <Combobox
                inputValue={handle}
                onInputValueChange={(value, details) => {
                  if (details.reason === "input-change") {
                    setHandle(value);
                  }
                }}
                onValueChange={(value) => {
                  if (value) {
                    const actor = actorsByHandle.get(value as string);
                    if (actor) {
                      setSelectedActor(actor);
                      setHandle(actor.handle);
                    } else {
                      setHandle(value as string);
                    }
                  }
                }}
                open={showDropdown}
                onOpenChange={setDropdownOpen}
                items={actors.map((a) => a.handle)}
                filter={null}
              >
                <ComboboxInput
                  className="w-full"
                  id="handle"
                  placeholder="you.bsky.social"
                  disabled={loading}
                  showTrigger={false}
                />
                <ComboboxContent className="min-w-(--anchor-width)">
                  <ComboboxList>
                    {(item: string) => {
                      const actor = actorsByHandle.get(item);
                      return (
                        <ComboboxItem key={item} value={item}>
                          <Avatar size="sm">
                            {actor?.avatar && (
                              <AvatarImage src={actor.avatar} alt={item} />
                            )}
                            <AvatarFallback>
                              {(
                                actor?.displayName?.[0] ?? item[0]
                              ).toUpperCase()}
                            </AvatarFallback>
                          </Avatar>
                          <div className="flex flex-col">
                            {actor?.displayName && (
                              <span className="text-sm font-medium leading-tight">
                                {actor.displayName}
                              </span>
                            )}
                            <span className="text-muted-foreground text-xs">
                              @{item}
                            </span>
                          </div>
                        </ComboboxItem>
                      );
                    }}
                  </ComboboxList>
                </ComboboxContent>
              </Combobox>
            )}
          </Field>
          <Field>
            <Button
              type="submit"
              className="w-full"
              disabled={loading || (!selectedActor && !handle.trim())}
            >
              {loading ? "Signing in..." : "Sign in"}
            </Button>
          </Field>
        </FieldGroup>
      </form>
    </div>
  );
}
