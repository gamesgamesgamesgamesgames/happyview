// Completion, hover and signature help for one Lua editor, from the
// declarations the interpreter renders. Monaco's providers are global per
// language, so each answers only for the model of the editor that
// registered it, and two editors on a page do not double every suggestion.

import type { Monaco } from "@monaco-editor/react";
import type { editor, IDisposable, languages, Position } from "monaco-editor";

import { printType, type LuaDeclarations, type LuaFunction } from "./read";
import {
  membersOf,
  paramLabel,
  recordCollections,
  resolveChain,
  resolveMember,
  scriptBindings,
  signature,
  trailingChain,
} from "./resolve";

export interface LuaEditorState {
  decls: LuaDeclarations;
  /** The script's trigger id, which types `input` and `ctx`. */
  trigger?: string;
}

function textBefore(model: editor.ITextModel, position: Position): string {
  return model
    .getLineContent(position.lineNumber)
    .substring(0, position.column - 1);
}

function wordRange(model: editor.ITextModel, position: Position) {
  const word = model.getWordUntilPosition(position);
  return {
    startLineNumber: position.lineNumber,
    endLineNumber: position.lineNumber,
    startColumn: word.startColumn,
    endColumn: word.endColumn,
  };
}

/** The open call around the cursor: the text before its `(`, and which
 * argument the cursor is in. */
function openCall(
  before: string,
): { callee: string; argument: number } | undefined {
  let depth = 0;
  let argument = 0;
  let inString: string | undefined;
  for (let i = before.length - 1; i >= 0; i--) {
    const ch = before[i];
    if (inString) {
      if (ch === inString) inString = undefined;
      continue;
    }
    if (ch === '"' || ch === "'") inString = ch;
    else if (ch === ")" || ch === "}") depth++;
    else if (ch === "{" && depth > 0) depth--;
    else if (ch === "(") {
      if (depth === 0) return { callee: before.slice(0, i), argument };
      depth--;
    } else if (ch === "," && depth === 0) argument++;
  }
  return undefined;
}

function doc(text: string | undefined) {
  return text ? { value: text } : undefined;
}

export function registerLuaProviders(
  monaco: Monaco,
  owns: (model: editor.ITextModel) => boolean,
  state: () => LuaEditorState,
): IDisposable[] {
  const kinds = monaco.languages.CompletionItemKind;

  const members = monaco.languages.registerCompletionItemProvider("lua", {
    triggerCharacters: [".", ":", '"', "'"],
    provideCompletionItems(model: editor.ITextModel, position: Position) {
      if (!owns(model)) return { suggestions: [] };
      const { decls, trigger } = state();
      const before = textBefore(model, position);
      const range = wordRange(model, position);

      // require("… — the modules the libraries provide.
      const required = /\brequire\s*\(?\s*["']([\w.]*)$/.exec(before);
      if (required) {
        const start = position.column - required[1].length;
        return {
          suggestions: [...decls.modules.keys()].sort().map((name) => ({
            label: name,
            kind: kinds.Module,
            insertText: name,
            range: { ...range, startColumn: start },
          })),
        };
      }

      // A string argument to a parameter named `collection`.
      const quoted = /["']([\w.]*)$/.exec(before);
      if (quoted) {
        const call = openCall(
          before.slice(0, before.length - quoted[0].length),
        );
        const chain = call && trailingChain(call.callee);
        const bindings =
          chain && scriptBindings(decls, model.getValue(), trigger);
        const fn =
          chain && bindings && chain.length > 1
            ? resolveMember(decls, bindings, chain)?.fn
            : undefined;
        if (fn?.params[call!.argument]?.name === "collection") {
          const start = position.column - quoted[1].length;
          return {
            suggestions: recordCollections(decls).map((nsid) => ({
              label: nsid,
              kind: kinds.Value,
              insertText: nsid,
              range: { ...range, startColumn: start },
            })),
          };
        }
        return { suggestions: [] };
      }

      // a.b. or a:b — the members of whatever comes before the separator.
      const sep = /([.:])\s*(\w*)$/.exec(before);
      if (!sep) return { suggestions: [] };
      const chain = trailingChain(
        before.slice(0, before.length - sep[0].length),
      );
      if (!chain) return { suggestions: [] };
      const bindings = scriptBindings(decls, model.getValue(), trigger);
      const owner = resolveChain(decls, bindings, chain);
      const found = owner && membersOf(decls, owner);
      if (!found) return { suggestions: [] };
      const colon = sep[1] === ":";
      const suggestions: languages.CompletionItem[] = [];
      if (!colon) {
        for (const field of found.fields) {
          suggestions.push({
            label: field.name,
            kind: kinds.Field,
            detail: printType(field.type),
            documentation: doc(field.description),
            insertText: /^\w+$/.test(field.name)
              ? field.name
              : `["${field.name}"]`,
            range,
          });
        }
      }
      for (const fn of found.functions.filter((f) => f.method === colon)) {
        suggestions.push({
          label: fn.name,
          kind: colon ? kinds.Method : kinds.Function,
          detail: signature(fn),
          documentation: doc(fn.description),
          insertText: fn.name,
          range,
        });
      }
      return { suggestions };
    },
  });

  const hover = monaco.languages.registerHoverProvider("lua", {
    provideHover(model: editor.ITextModel, position: Position) {
      if (!owns(model)) return null;
      const word = model.getWordAtPosition(position);
      if (!word) return null;
      const { decls, trigger } = state();
      const line = model.getLineContent(position.lineNumber);
      const chain = trailingChain(line.substring(0, word.endColumn - 1));
      if (!chain) return null;
      const bindings = scriptBindings(decls, model.getValue(), trigger);
      const range = {
        startLineNumber: position.lineNumber,
        endLineNumber: position.lineNumber,
        startColumn: word.startColumn,
        endColumn: word.endColumn,
      };
      if (chain.length === 1) {
        const type = bindings.get(chain[0].name);
        return type
          ? {
              range,
              contents: [
                {
                  value:
                    "```lua\n" +
                    `${chain[0].name}: ${printType(type)}` +
                    "\n```",
                },
              ],
            }
          : null;
      }
      const member = resolveMember(decls, bindings, chain);
      if (member?.fn) {
        return {
          range,
          contents: [
            { value: "```lua\n" + signature(member.fn) + "\n```" },
            ...(member.fn.description
              ? [{ value: member.fn.description }]
              : []),
          ],
        };
      }
      if (member?.field) {
        const f = member.field;
        return {
          range,
          contents: [
            {
              value:
                "```lua\n" +
                `(field) ${f.name}${f.optional ? "?" : ""}: ${printType(f.type)}` +
                "\n```",
            },
            ...(f.description ? [{ value: f.description }] : []),
          ],
        };
      }
      return null;
    },
  });

  const signatures = monaco.languages.registerSignatureHelpProvider("lua", {
    signatureHelpTriggerCharacters: ["(", ","],
    provideSignatureHelp(model: editor.ITextModel, position: Position) {
      if (!owns(model)) return null;
      const { decls, trigger } = state();
      const call = openCall(textBefore(model, position));
      const chain = call && trailingChain(call.callee);
      if (!call || !chain || chain.length < 2) return null;
      const fn: LuaFunction | undefined = resolveMember(
        decls,
        scriptBindings(decls, model.getValue(), trigger),
        chain,
      )?.fn;
      if (!fn || fn.params.length === 0) return null;
      return {
        value: {
          signatures: [
            {
              label: signature(fn),
              documentation: doc(fn.description),
              parameters: fn.params.map((p) => ({
                label: paramLabel(p),
                documentation: doc(p.description),
              })),
            },
          ],
          activeSignature: 0,
          activeParameter: Math.min(call.argument, fn.params.length - 1),
        },
        dispose() {},
      };
    },
  });

  return [members, hover, signatures];
}
