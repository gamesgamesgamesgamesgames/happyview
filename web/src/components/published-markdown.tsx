"use client";

import { isValidElement, type ComponentProps, type ReactNode } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";

import { useHighlighted } from "@/lib/highlight";
import { cn } from "@/lib/utils";

const FENCE_CODE_CLASS = "font-mono text-xs whitespace-pre";

/** The fence's language, or null for an indented block or a bare fence. */
function fenceGrammar(className: unknown): string | null {
  const match =
    typeof className === "string"
      ? /(?:^|\s)language-(\S+)/.exec(className)
      : null;
  return match ? match[1] : null;
}

/** The code, which arrives as one string or a list of them. */
function textOf(node: ReactNode): string {
  if (typeof node === "string") return node;
  if (Array.isArray(node)) return node.map(textOf).join("");
  return "";
}

/**
 * `not-prose` because the block styles itself: prose's own `pre` rules would
 * draw a second box inside this one, and `shiki-on-surface` has already given
 * up the theme's background so the surface shows through.
 */
function Fence({ code, grammar }: { code: string; grammar: string | null }) {
  const nodes = useHighlighted(code, grammar, FENCE_CODE_CLASS);
  return (
    <div className="not-prose shiki-on-surface my-4 overflow-x-auto rounded-lg border bg-muted/40 px-4 py-3">
      {nodes ?? (
        <pre>
          <code className={FENCE_CODE_CLASS}>{code}</code>
        </pre>
      )}
    </div>
  );
}

/**
 * Every code block CommonMark produces is a `pre` wrapping a single `code`, so
 * the child carries both the text and the fence's language class.
 */
function Pre({ children }: ComponentProps<"pre">) {
  if (!isValidElement<ComponentProps<"code">>(children)) {
    return <pre>{children}</pre>;
  }
  const { className, children: body } = children.props;
  return (
    <Fence
      // The text ends in the newline that closed the fence, which would
      // otherwise highlight as a trailing blank line.
      code={textOf(body).replace(/\n$/, "")}
      grammar={fenceGrammar(className)}
    />
  );
}

/**
 * CommonMark written by a plugin's publisher and shown to an operator.
 *
 * The registry's lexicons require it to be rendered as untrusted input
 * (`at.happyproto.plugin.v1`'s `sections`, `at.happyproto.registry.defs`'s
 * `changelogView`): raw HTML is not rendered, and a link scheme the renderer
 * does not recognise is refused. Both hold here by construction — no `rehype-raw`, so embedded HTML
 * stays text, and react-markdown's default URL transform passes only the safe
 * schemes — so this component exists to keep that true in one place rather
 * than at each call site. Highlighting does not widen that surface: Shiki is
 * handed the fence's text and its output is built into elements, never parsed
 * as HTML.
 */
export function PublishedMarkdown({
  children,
  className,
}: {
  children: string;
  className?: string;
}) {
  return (
    <div
      className={cn(
        "prose prose-sm dark:prose-invert max-w-none",
        // Prose sets its own mono styling for inline code; keep it from
        // outgrowing the surrounding type. Fenced blocks opt out of prose
        // entirely and size themselves.
        "prose-code:text-xs",
        className,
      )}
    >
      <ReactMarkdown remarkPlugins={[remarkGfm]} components={{ pre: Pre }}>
        {children}
      </ReactMarkdown>
    </div>
  );
}
