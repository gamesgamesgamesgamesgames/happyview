"use client";

import { useEffect, useMemo, useRef } from "react";
import dynamic from "next/dynamic";
import { useTheme } from "next-themes";
import type { editor, IDisposable, Position } from "monaco-editor";
import type { Monaco } from "@monaco-editor/react";
import { LUA_KEYWORDS, LUA_BUILTINS, LUA_SNIPPETS } from "@/lib/lua-language";
import { lexiconJsonSchema, LEXICON_SCHEMA_URI } from "@/lib/lexicon-schema";
import { resolveCssColor } from "@/lib/css-utils";
import { parseLuaIdentifiers } from "@/lib/lua-parser";
import { HOVER_DOCS } from "@/lib/lua-hover";
import { readDeclarations } from "@/lib/luals/read";
import { registerLuaProviders, type LuaEditorState } from "@/lib/luals/monaco";
import { useDeclarations } from "@/hooks/use-declarations";

const Editor = dynamic(() => import("@monaco-editor/react"), { ssr: false });
const DiffEditor = dynamic(
  () => import("@monaco-editor/react").then((m) => m.DiffEditor),
  { ssr: false },
);

/**
 * Registers the dark theme both `MonacoEditor` and `MonacoDiffEditor` use,
 * reading the sidebar background so the editor's chrome matches the shell.
 * Idempotent — `defineTheme` overwrites, so re-registering per mount is fine.
 */
function defineHappyViewDarkTheme(monaco: Monaco) {
  const bg = resolveCssColor("var(--sidebar)");
  monaco.editor.defineTheme("happyview-dark", {
    base: "vs-dark",
    inherit: true,
    rules: [],
    colors: { "editor.background": bg },
  });
}

/** Languages whose interpreter renders declarations an editor can use. */
const TYPED_LANGUAGES = new Set(["lua", "typescript", "javascript"]);

interface MonacoEditorProps {
  value: string;
  onChange?: (value: string) => void;
  language: string;
  readOnly?: boolean;
  className?: string;
  /** The script's trigger id, which types a handler's `input` and `ctx`. */
  trigger?: string;
  tabSize?: number;
}

export function MonacoEditor({
  value,
  onChange,
  language,
  readOnly,
  className,
  trigger,
  tabSize = 2,
}: MonacoEditorProps) {
  const { resolvedTheme } = useTheme();
  const files = useDeclarations(
    TYPED_LANGUAGES.has(language) ? language : undefined,
  );
  const decls = useMemo(() => readDeclarations(files), [files]);
  const luaState = useRef<LuaEditorState>({ decls, trigger });
  const monacoRef = useRef<Monaco | null>(null);
  const editorRef = useRef<editor.IStandaloneCodeEditor | null>(null);
  const disposablesRef = useRef<IDisposable[]>([]);

  // Providers read the latest declarations and trigger through the ref, so
  // they are registered once per mount.
  useEffect(() => {
    luaState.current = { decls, trigger };
  }, [decls, trigger]);

  useEffect(() => {
    return () => {
      for (const d of disposablesRef.current) d.dispose();
      disposablesRef.current = [];
    };
  }, []);

  // TypeScript and JavaScript read the files as ambient modules, so
  // `import type { QueryCtx } from "happyview:ctx"` resolves.
  useEffect(() => {
    const monaco = monacoRef.current;
    if (!monaco || (language !== "typescript" && language !== "javascript"))
      return;
    const defaults =
      language === "typescript"
        ? monaco.typescript.typescriptDefaults
        : monaco.typescript.javascriptDefaults;
    const libs = files
      .filter((f) => f.path.endsWith(".d.ts"))
      .map((f) =>
        defaults.addExtraLib(f.contents, `file:///happyview-types/${f.path}`),
      );
    return () => libs.forEach((l) => l.dispose());
  }, [files, language]);

  const owns = (model: editor.ITextModel) =>
    model === editorRef.current?.getModel();

  return (
    <div className={`relative ${className ?? ""}`}>
      <div className="absolute inset-0">
        <Editor
          height="100%"
          language={language}
          value={value}
          theme={resolvedTheme === "dark" ? "happyview-dark" : "vs"}
          onChange={(v) => onChange?.(v ?? "")}
          loading="Loading editor..."
          path={language === "json" ? "lexicon.json" : undefined}
          beforeMount={(monaco) => {
            defineHappyViewDarkTheme(monaco);

            // Configure JSON language service with Lexicon schema
            if (language === "json") {
              monaco.languages.json.jsonDefaults.setDiagnosticsOptions({
                validate: true,
                allowComments: false,
                trailingCommas: "error",
                schemas: [
                  {
                    uri: LEXICON_SCHEMA_URI,
                    fileMatch: ["lexicon.json"],
                    schema: lexiconJsonSchema,
                  },
                ],
              });
            }
          }}
          onMount={(mounted, monaco) => {
            monacoRef.current = monaco;
            editorRef.current = mounted;
            if (language !== "lua") return;

            // What HappyView's libraries, lexicons and ctx offer.
            disposablesRef.current.push(
              ...registerLuaProviders(monaco, owns, () => luaState.current),
            );

            // Lua keywords, builtins, snippets and the script's own names.
            disposablesRef.current.push(
              monaco.languages.registerCompletionItemProvider("lua", {
                provideCompletionItems(
                  model: editor.ITextModel,
                  position: Position,
                ) {
                  if (!owns(model)) return { suggestions: [] };
                  const word = model.getWordUntilPosition(position);
                  const range = {
                    startLineNumber: position.lineNumber,
                    endLineNumber: position.lineNumber,
                    startColumn: word.startColumn,
                    endColumn: word.endColumn,
                  };

                  const lineContent = model.getLineContent(position.lineNumber);
                  const textBeforeCursor = lineContent.substring(
                    0,
                    position.column - 1,
                  );

                  // Member access and strings are the other provider's.
                  if (/[.:]\s*\w*$/.test(textBeforeCursor)) {
                    return { suggestions: [] };
                  }
                  if (/["'][\w.]*$/.test(textBeforeCursor)) {
                    return { suggestions: [] };
                  }

                  const staticLabels = new Set([
                    ...LUA_KEYWORDS,
                    ...LUA_BUILTINS,
                    ...LUA_SNIPPETS.map((s) => s.label),
                  ]);
                  const identifiers = parseLuaIdentifiers(model.getValue());
                  // Remove the word currently being typed
                  if (word.word) identifiers.delete(word.word);
                  for (const name of ["input", "ctx"]) {
                    if (luaState.current.trigger) identifiers.add(name);
                  }

                  const suggestions = [
                    ...LUA_KEYWORDS.map((kw) => ({
                      label: kw,
                      kind: monaco.languages.CompletionItemKind.Keyword,
                      insertText: kw,
                      detail: "keyword",
                      range,
                    })),
                    ...LUA_BUILTINS.map((fn) => ({
                      label: fn,
                      kind: monaco.languages.CompletionItemKind.Function,
                      insertText: fn,
                      detail: "function",
                      range,
                    })),
                    ...LUA_SNIPPETS.map((snip) => ({
                      label: snip.label,
                      kind: monaco.languages.CompletionItemKind.Snippet,
                      insertText: snip.insertText,
                      insertTextRules:
                        monaco.languages.CompletionItemInsertTextRule
                          .InsertAsSnippet,
                      detail: snip.detail,
                      documentation: snip.description,
                      range,
                    })),
                    ...[...identifiers]
                      .filter((id) => !staticLabels.has(id))
                      .map((id) => ({
                        label: id,
                        kind: monaco.languages.CompletionItemKind.Variable,
                        insertText: id,
                        detail: "identifier",
                        range,
                      })),
                  ];

                  return { suggestions };
                },
              }),
            );

            // Lua's own builtins: `string.format`, `table.insert`, ...
            disposablesRef.current.push(
              monaco.languages.registerHoverProvider("lua", {
                provideHover(model: editor.ITextModel, position: Position) {
                  if (!owns(model)) return null;
                  const word = model.getWordAtPosition(position);
                  if (!word) return null;

                  const lineContent = model.getLineContent(position.lineNumber);
                  const charBefore = lineContent[word.startColumn - 2];
                  let key = word.word;

                  if (charBefore === "." || charBefore === ":") {
                    // Find the module/object prefix before the dot/colon
                    const textBefore = lineContent.substring(
                      0,
                      word.startColumn - 2,
                    );
                    const prefixMatch = textBefore.match(/(\w+)$/);
                    if (prefixMatch) {
                      const sep = charBefore === ":" ? ":" : ".";
                      key = `${prefixMatch[1]}${sep}${word.word}`;
                    }
                  }

                  const entry = HOVER_DOCS.get(key);
                  if (!entry) return null;

                  const range = {
                    startLineNumber: position.lineNumber,
                    endLineNumber: position.lineNumber,
                    startColumn: word.startColumn,
                    endColumn: word.endColumn,
                  };

                  return {
                    range,
                    contents: [
                      { value: `\`\`\`lua\n${entry.signature}\n\`\`\`` },
                      { value: entry.description },
                    ],
                  };
                },
              }),
            );

            disposablesRef.current.push(
              monaco.languages.registerSignatureHelpProvider("lua", {
                signatureHelpTriggerCharacters: ["(", ","],
                provideSignatureHelp(
                  model: editor.ITextModel,
                  position: Position,
                ) {
                  if (!owns(model)) return null;
                  const lineContent = model.getLineContent(position.lineNumber);
                  const textBeforeCursor = lineContent.substring(
                    0,
                    position.column - 1,
                  );

                  // Walk backward to find the opening ( and the function name
                  let depth = 0;
                  let parenPos = -1;
                  let activeParam = 0;
                  for (let i = textBeforeCursor.length - 1; i >= 0; i--) {
                    const ch = textBeforeCursor[i];
                    if (ch === ")") depth++;
                    else if (ch === "(") {
                      if (depth === 0) {
                        parenPos = i;
                        break;
                      }
                      depth--;
                    } else if (ch === "," && depth === 0) {
                      activeParam++;
                    }
                  }

                  if (parenPos < 0) return null;

                  // Extract the function name before the (
                  const textBeforeParen = textBeforeCursor.substring(
                    0,
                    parenPos,
                  );
                  const fnMatch = textBeforeParen.match(
                    /([\w.]+[:.]\w+|\w+)\s*$/,
                  );
                  if (!fnMatch) return null;

                  const fnName = fnMatch[1];
                  // `s:format(...)` is `string.format`.
                  const entry =
                    HOVER_DOCS.get(fnName) ??
                    HOVER_DOCS.get(fnName.replace(/^\w+:/, "string."));
                  if (!entry) return null;

                  // Parse parameters from the signature: extract content inside parens
                  const sigParenMatch = entry.signature.match(/\(([^)]*)\)/);
                  if (!sigParenMatch) return null;

                  const paramString = sigParenMatch[1].trim();
                  if (!paramString) return null;

                  // Split parameters on commas, respecting brackets
                  const params: string[] = [];
                  let current = "";
                  let bracketDepth = 0;
                  for (const ch of paramString) {
                    if (ch === "[") bracketDepth++;
                    else if (ch === "]") bracketDepth--;
                    else if (ch === "," && bracketDepth === 0) {
                      params.push(current.trim());
                      current = "";
                      continue;
                    }
                    current += ch;
                  }
                  if (current.trim()) params.push(current.trim());

                  return {
                    value: {
                      signatures: [
                        {
                          label: entry.signature,
                          documentation: entry.description,
                          parameters: params.map((p) => ({
                            label: p
                              .replace(/^\[?\s*/, "")
                              .replace(/\s*\]?\s*$/, ""),
                          })),
                        },
                      ],
                      activeSignature: 0,
                      activeParameter: Math.min(activeParam, params.length - 1),
                    },
                    dispose() {},
                  };
                },
              }),
            );
          }}
          options={{
            readOnly,
            minimap: { enabled: false },
            automaticLayout: true,
            scrollBeyondLastLine: false,
            wordWrap: "on",
            fontSize: 12,
            tabSize,
            snippetSuggestions: "inline",
            renderLineHighlight: readOnly ? "none" : "line",
            hideCursorInOverviewRuler: readOnly,
            overviewRulerLanes: readOnly ? 0 : 3,
            quickSuggestions: true,
          }}
        />
      </div>
    </div>
  );
}

interface MonacoDiffEditorProps {
  /** Left ("before") side. */
  original: string;
  /** Right ("after") side. */
  modified: string;
  language: string;
  className?: string;
}

/** Read-only side-by-side diff, styled to match {@link MonacoEditor}. */
export function MonacoDiffEditor({
  original,
  modified,
  language,
  className,
}: MonacoDiffEditorProps) {
  const { resolvedTheme } = useTheme();

  return (
    <div className={`relative ${className ?? ""}`}>
      <div className="absolute inset-0">
        <DiffEditor
          height="100%"
          language={language}
          original={original}
          modified={modified}
          theme={resolvedTheme === "dark" ? "happyview-dark" : "vs"}
          loading="Loading diff..."
          beforeMount={defineHappyViewDarkTheme}
          options={{
            readOnly: true,
            minimap: { enabled: false },
            automaticLayout: true,
            scrollBeyondLastLine: false,
            wordWrap: "on",
            fontSize: 12,
            renderSideBySide: true,
          }}
        />
      </div>
    </div>
  );
}
