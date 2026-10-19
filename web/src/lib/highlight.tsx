"use client";

import {
  Fragment,
  type ComponentProps,
  type ReactNode,
  useEffect,
  useState,
} from "react";
import { jsx, jsxs } from "react/jsx-runtime";
import { toJsxRuntime } from "hast-util-to-jsx-runtime";
// The full bundle rather than `shiki/bundle/web`, which carries no Lua
// grammar — the language that most needs highlighting here. Grammars load on
// demand, so only the ones actually rendered are fetched.
import { codeToHast, type BundledLanguage } from "shiki";

export const CODE_CLASS = "font-mono text-sm whitespace-pre";

async function highlight(
  code: string,
  grammar: string,
  codeClassName: string,
): Promise<ReactNode> {
  const hast = await codeToHast(code, {
    lang: grammar as BundledLanguage,
    themes: { light: "github-light", dark: "github-dark" },
    // The `.shiki` rules in globals.css read the per-theme custom properties
    // this emits, so the block follows the dashboard's own theme switch.
    defaultColor: false,
  });

  return toJsxRuntime(hast, {
    Fragment,
    jsx,
    jsxs,
    components: {
      pre: (props: ComponentProps<"pre">) => <pre {...props} />,
      code: (props: ComponentProps<"code">) => (
        <code className={codeClassName} {...props} />
      ),
    },
  }) as ReactNode;
}

/**
 * The highlighted code, or null until the grammar has loaded — and for good if
 * it never does, which is what makes the caller's plain fallback the contract
 * rather than a transient state.
 *
 * Every highlighted block comes through here. A second copy of the
 * `codeToHast` call would be a second pair of themes and a second answer to an
 * unknown grammar, and neither difference shows at a call site.
 *
 * A null `grammar` is highlighted as nothing rather than guessed at: an
 * unlabelled fence is not a Lua fence.
 */
export function useHighlighted(
  code: string,
  grammar: string | null,
  codeClassName: string = CODE_CLASS,
): ReactNode | null {
  const key = `${grammar}:${code}`;
  // The result carries the input it was produced from, so a result that
  // arrives after the input changed is ignored rather than cleared on the way
  // in — clearing would be a setState in the effect body.
  const [done, setDone] = useState<{ key: string; nodes: ReactNode } | null>(
    null,
  );

  useEffect(() => {
    if (grammar === null) return;
    let cancelled = false;
    void highlight(code, grammar, codeClassName)
      .then((nodes) => {
        if (!cancelled) setDone({ key, nodes });
      })
      // An unavailable grammar costs colour, not the code. A publisher's fence
      // can name anything at all, so this is a routine outcome here and not a
      // fault worth reporting.
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [code, grammar, codeClassName, key]);

  return done?.key === key ? done.nodes : null;
}
