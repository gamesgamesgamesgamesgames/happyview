"use client";

import { useEffect, useRef } from "react";
import dynamic from "next/dynamic";
import { useTheme } from "next-themes";
import type { editor, Position } from "monaco-editor";
import {
  LUA_KEYWORDS,
  LUA_BUILTINS,
  LUA_SNIPPETS,
  type LuaCompletions,
} from "@/lib/lua-completions";
import { lexiconJsonSchema, LEXICON_SCHEMA_URI } from "@/lib/lexicon-schema";
import { resolveCssColor } from "@/lib/css-utils";
import {
  parseLuaIdentifiers,
  parseRecordVariables,
  parseDbQueryVariables,
  parseDbQueryRecordIterators,
} from "@/lib/lua-parser";
import { HOVER_DOCS } from "@/lib/lua-hover";

const Editor = dynamic(() => import("@monaco-editor/react"), { ssr: false });

interface MonacoEditorProps {
  value: string;
  onChange?: (value: string) => void;
  language: string;
  readOnly?: boolean;
  className?: string;
  completions?: LuaCompletions;
  collections?: string[];
  tabSize?: number;
}

export function MonacoEditor({
  value,
  onChange,
  language,
  readOnly,
  className,
  completions,
  collections,
  tabSize = 2,
}: MonacoEditorProps) {
  const { resolvedTheme } = useTheme();
  const completionsRef = useRef(completions);
  const collectionsRef = useRef(collections);
  const disposablesRef = useRef<{ dispose(): void }[]>([]);

  // eslint-disable-next-line react-hooks/refs -- see comment above
  completionsRef.current = completions;
  // eslint-disable-next-line react-hooks/refs -- see comment above
  collectionsRef.current = collections;

  useEffect(() => {
    return () => {
      for (const d of disposablesRef.current) d.dispose();
      disposablesRef.current = [];
    };
  }, []);

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
            const bg = resolveCssColor("var(--sidebar)");
            monaco.editor.defineTheme("happyview-dark", {
              base: "vs-dark",
              inherit: true,
              rules: [],
              colors: { "editor.background": bg },
            });

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
          onMount={(_editor, monaco) => {
            if (language !== "lua") return;

            // Provider 1: Lua keywords, builtins, and snippets
            disposablesRef.current.push(
              monaco.languages.registerCompletionItemProvider("lua", {
                provideCompletionItems(
                  model: editor.ITextModel,
                  position: Position,
                ) {
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

                  // Don't suggest keywords after . or : (those are member access)
                  if (/[.:]$/.test(textBeforeCursor.trimEnd())) {
                    return { suggestions: [] };
                  }

                  // Extract identifiers from the current document
                  const staticLabels = new Set([
                    ...LUA_KEYWORDS,
                    ...LUA_BUILTINS,
                    ...LUA_SNIPPETS.map((s) => s.label),
                  ]);
                  const fullSource = model.getValue();
                  const identifiers = parseLuaIdentifiers(fullSource);
                  // Remove the word currently being typed
                  if (word.word) identifiers.delete(word.word);

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

            // Provider 3: Hover documentation
            disposablesRef.current.push(
              monaco.languages.registerHoverProvider("lua", {
                provideHover(model: editor.ITextModel, position: Position) {
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

            // Provider 4: Signature help
            disposablesRef.current.push(
              monaco.languages.registerSignatureHelpProvider("lua", {
                signatureHelpTriggerCharacters: ["(", ","],
                provideSignatureHelp(
                  model: editor.ITextModel,
                  position: Position,
                ) {
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
                  // Normalize colon to look up both Record:save and Record.save
                  const entry =
                    HOVER_DOCS.get(fnName) ??
                    HOVER_DOCS.get(fnName.replace(":", "."));
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

            // Provider 2: HappyView-specific completions (Record, db, collections)
            if (!completions) return;
            disposablesRef.current.push(
              monaco.languages.registerCompletionItemProvider("lua", {
                triggerCharacters: [".", ":", '"'],
                provideCompletionItems(
                  model: editor.ITextModel,
                  position: Position,
                ) {
                  const lineContent = model.getLineContent(position.lineNumber);
                  const textBeforeCursor = lineContent.substring(
                    0,
                    position.column - 1,
                  );

                  // Inside db.query/search/backlinks({ ... }) — suggest option keys
                  const dbOptionsMatch = textBeforeCursor.match(
                    /db\.(query|search|backlinks)\(\s*\{[^}]*$/,
                  );
                  if (dbOptionsMatch) {
                    const optionEntries =
                      completionsRef.current?.[`db.${dbOptionsMatch[1]}`] ?? [];
                    if (optionEntries.length) {
                      // Check for collection = " inside db.query — offer NSID completions
                      const collectionQuoteMatch = textBeforeCursor.match(
                        /collection\s*=\s*"([^"]*)$/,
                      );
                      if (collectionQuoteMatch) {
                        const cols = collectionsRef.current;
                        if (!cols?.length) return { suggestions: [] };
                        const quoteCol = textBeforeCursor.lastIndexOf('"');
                        const range = {
                          startLineNumber: position.lineNumber,
                          endLineNumber: position.lineNumber,
                          startColumn: quoteCol + 2,
                          endColumn: position.column,
                        };
                        return {
                          suggestions: cols.map((col) => ({
                            label: col,
                            kind: monaco.languages.CompletionItemKind.Value,
                            insertText: col,
                            range,
                          })),
                        };
                      }

                      const word = model.getWordUntilPosition(position);
                      const range = {
                        startLineNumber: position.lineNumber,
                        endLineNumber: position.lineNumber,
                        startColumn: word.startColumn,
                        endColumn: word.endColumn,
                      };
                      return {
                        suggestions: optionEntries.map((entry) => ({
                          label: entry.label,
                          kind: monaco.languages.CompletionItemKind.Property,
                          detail: entry.detail,
                          documentation: entry.description,
                          insertText: entry.label,
                          range,
                        })),
                      };
                    }
                  }

                  // Collection NSID completions for collection = " (outside db options) and db.count
                  const collectionAssignMatch = textBeforeCursor.match(
                    /(?:db\.count\(\s*"([^"]*)$|collection\s*=\s*"([^"]*)$)/,
                  );

                  // Record("...") collection completions
                  const recordMatch =
                    textBeforeCursor.match(/Record\(\s*"([^"]*)$/);
                  if (recordMatch || collectionAssignMatch) {
                    const cols = collectionsRef.current;
                    if (!cols?.length) return { suggestions: [] };
                    const quoteCol = textBeforeCursor.lastIndexOf('"');
                    const range = {
                      startLineNumber: position.lineNumber,
                      endLineNumber: position.lineNumber,
                      startColumn: quoteCol + 2, // after the opening "
                      endColumn: position.column,
                    };
                    return {
                      suggestions: cols.map((col) => ({
                        label: col,
                        kind: monaco.languages.CompletionItemKind.Value,
                        insertText: col,
                        range,
                      })),
                    };
                  }

                  // Dot or colon-triggered completions (Record., r:, db., etc.)
                  const dotMatch = textBeforeCursor.match(/(\w+)\.\w*$/);
                  const colonMatch = textBeforeCursor.match(/(\w+):\w*$/);
                  const match = dotMatch || colonMatch;
                  const isColon = !!colonMatch;
                  if (!match) return { suggestions: [] };

                  const prefix = match[1];

                  // Build entries based on context
                  let entries = completionsRef.current?.[prefix];

                  if (!entries && prefix !== "Record" && prefix !== "db") {
                    const fullSource = model.getValue();

                    // Check if it's a db.query() result variable
                    const dbQueryVars = parseDbQueryVariables(fullSource);
                    if (prefix in dbQueryVars) {
                      entries =
                        completionsRef.current?.["db.query_result"] ?? [];
                    }

                    // Check if it's an iterator over db.query().records
                    if (!entries) {
                      const iterMap = parseDbQueryRecordIterators(
                        fullSource,
                        dbQueryVars,
                      );
                      const iterCollection = iterMap[prefix];
                      if (iterCollection) {
                        const schemaEntries =
                          completionsRef.current?.[iterCollection] ?? [];
                        const uriEntry = {
                          label: "uri",
                          detail: "string",
                          description: "AT URI of the record",
                        };
                        entries = [uriEntry, ...schemaEntries];
                      }
                    }

                    if (!entries) {
                      // Variable access — check if it's a Record variable
                      const varMap = parseRecordVariables(fullSource);
                      const collection = varMap[prefix];

                      if (collection) {
                        // Record instance methods/fields
                        const instanceEntries =
                          completionsRef.current?.["Record"]?.filter(
                            (e) =>
                              e.detail === "method" || e.detail?.endsWith("?"),
                          ) ?? [];

                        // Collection-specific record properties (merged into completions by parent)
                        const schemaEntries =
                          completionsRef.current?.[collection] ?? [];

                        entries = [...instanceEntries, ...schemaEntries];
                      }
                    }
                  }

                  if (isColon && !entries) {
                    // Colon on unknown variable — show Record instance methods + string methods
                    const recordMethods =
                      completionsRef.current?.["Record"]?.filter(
                        (e) => e.detail === "method",
                      ) ?? [];
                    const stringMethods =
                      completionsRef.current?.["string"] ?? [];
                    entries = [...recordMethods, ...stringMethods];
                  }

                  if (!entries?.length) return { suggestions: [] };

                  const word = model.getWordUntilPosition(position);
                  const range = {
                    startLineNumber: position.lineNumber,
                    endLineNumber: position.lineNumber,
                    startColumn: word.startColumn,
                    endColumn: word.endColumn,
                  };

                  return {
                    suggestions: entries.map((entry) => ({
                      label: entry.label,
                      kind:
                        entry.detail === "method" || entry.detail === "function"
                          ? monaco.languages.CompletionItemKind.Method
                          : monaco.languages.CompletionItemKind.Field,
                      detail: entry.detail,
                      documentation: entry.description,
                      insertText: entry.insertText ?? entry.label,
                      ...(entry.insertText
                        ? {
                            insertTextRules:
                              monaco.languages.CompletionItemInsertTextRule
                                .InsertAsSnippet,
                          }
                        : {}),
                      range,
                    })),
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
