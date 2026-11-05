"use client";

import { useState } from "react";
import { cn } from "@/lib/utils";
import { MonacoEditor } from "@/components/monaco-editor";

interface CodePanelsProps {
  jsonValue: string;
  onJsonChange?: (value: string) => void;
  jsonReadOnly?: boolean;
  luaValue?: string | null;
  onLuaChange?: (value: string) => void;
  luaReadOnly?: boolean;
  className?: string;
}

function Panel({
  className,
  label,
  value,
  onChange,
  readOnly,
  lang,
}: {
  className?: string;
  label: string;
  value: string;
  onChange?: (value: string) => void;
  readOnly?: boolean;
  lang: string;
}) {
  return (
    <div className={cn("flex flex-col min-h-0 flex-1", className)}>
      <div className="bg-sidebar-accent p-2 text-xs">{label}</div>
      <MonacoEditor
        className="min-h-[200px] flex-1 overflow-hidden"
        value={value}
        onChange={onChange}
        language={lang}
        readOnly={readOnly || !onChange}
      />
    </div>
  );
}

export function CodePanels({
  jsonValue,
  onJsonChange,
  jsonReadOnly,
  luaValue,
  onLuaChange,
  luaReadOnly,
  className,
}: CodePanelsProps) {
  const hasLua = luaValue != null || !!onLuaChange;
  const [activeTab, setActiveTab] = useState<"json" | "lua">("json");

  return (
    <div className={cn("flex flex-col min-h-0", className)}>
      {/* Narrow-screen tab switcher (only when Lua panel exists) */}
      {hasLua && (
        <div className="lg:hidden flex gap-1 mb-4 rounded-lg bg-muted p-1">
          <button
            className={cn(
              "flex-1 rounded-md px-3 py-1.5 text-sm font-medium transition-colors",
              activeTab === "json"
                ? "bg-background text-foreground shadow-sm"
                : "text-muted-foreground hover:text-foreground",
            )}
            onClick={() => setActiveTab("json")}
          >
            Lexicon JSON
          </button>
          <button
            className={cn(
              "flex-1 rounded-md px-3 py-1.5 text-sm font-medium transition-colors",
              activeTab === "lua"
                ? "bg-background text-foreground shadow-sm"
                : "text-muted-foreground hover:text-foreground",
            )}
            onClick={() => setActiveTab("lua")}
          >
            Lua Script
          </button>
        </div>
      )}

      {/* Single set of editors — CSS controls narrow/wide layout */}
      <div
        className="border flex flex-col lg:grid flex-1 min-h-0 overflow-hidden rounded-md"
        style={{
          gridTemplateColumns: hasLua ? "1fr 1fr" : "1fr 0fr",
          transition: "grid-template-columns 300ms ease-in-out",
        }}
      >
        <Panel
          className={cn(
            /* Narrow: hide when Lua tab is active */
            hasLua && activeTab !== "json" ? "hidden lg:flex" : "",
            /* Wide: dim inactive panel */
            hasLua
              ? "lg:border-e lg:opacity-50 lg:focus-within:opacity-100 lg:transition-opacity"
              : "",
          )}
          label="Lexicon JSON"
          value={jsonValue}
          onChange={onJsonChange}
          readOnly={jsonReadOnly}
          lang="json"
        />
        {/* Wrapper stays in the DOM so the grid column can animate closed */}
        <div
          className={cn(
            "overflow-hidden min-w-0 flex flex-col min-h-0",
            /* Narrow: hide when JSON tab is active */
            hasLua && activeTab !== "lua"
              ? "hidden lg:flex"
              : hasLua
                ? ""
                : "hidden lg:flex",
          )}
        >
          {hasLua && (
            <Panel
              className="lg:opacity-50 lg:focus-within:opacity-100 lg:transition-opacity min-w-[300px]"
              label="Lua Script"
              value={luaValue ?? ""}
              onChange={onLuaChange}
              readOnly={luaReadOnly}
              lang="lua"
            />
          )}
        </div>
      </div>
    </div>
  );
}
