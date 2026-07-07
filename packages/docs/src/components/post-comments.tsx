'use client';

import { useEffect, useRef, useState } from 'react';
import Image from 'next/image';

interface Reply {
  uri: string;
  author: {
    did: string;
    handle: string;
    displayName?: string;
    avatar?: string;
  };
  record: {
    text: string;
    createdAt: string;
    facets?: Facet[];
  };
  likeCount?: number;
}

interface ParentPost {
  uri: string;
  author: Reply['author'];
  record: Reply['record'];
  likeCount: number;
  repostCount: number;
  replyCount: number;
}

interface Facet {
  index: { byteStart: number; byteEnd: number };
  features: FacetFeature[];
}

type FacetFeature =
  | { $type: 'app.bsky.richtext.facet#link'; uri: string }
  | { $type: 'app.bsky.richtext.facet#mention'; did: string }
  | { $type: string };

type LoadState = 'loading' | 'loaded' | 'error' | 'no-ref';

const PLATFORMS = [
  { key: 'bluesky', name: 'Bluesky', domain: 'bsky.app' },
  { key: 'blacksky', name: 'Blacksky', domain: 'blacksky.app' },
  { key: 'witchsky', name: 'Witchsky', domain: 'witchsky.app' },
  { key: 'mu', name: 'mu.social', domain: 'mu.social' },
] as const;

type PlatformKey = (typeof PLATFORMS)[number]['key'];

interface PostCommentsProps {
  atUri?: string;
}

export function PostComments({ atUri }: PostCommentsProps) {
  const [replies, setReplies] = useState<Reply[]>([]);
  const [parentPost, setParentPost] = useState<ParentPost | null>(null);
  const [postUri, setPostUri] = useState<string | null>(null);
  const [state, setState] = useState<LoadState>('loading');

  useEffect(() => {
    if (!atUri) {
      setState('no-ref');
      return;
    }

    setState('loading');
    resolveComments(atUri)
      .then((data) => {
        if (!data.bskyPostRef) {
          setState('no-ref');
          return;
        }
        setPostUri(data.bskyPostRef);
        setParentPost(data.parentPost);
        setReplies(data.replies);
        setState('loaded');
      })
      .catch(() => setState('error'));
  }, [atUri]);

  const retry = () => {
    setState('loading');
    if (!atUri) return;
    resolveComments(atUri)
      .then((data) => {
        if (!data.bskyPostRef) {
          setState('no-ref');
          return;
        }
        setPostUri(data.bskyPostRef);
        setParentPost(data.parentPost);
        setReplies(data.replies);
        setState('loaded');
      })
      .catch(() => setState('error'));
  };

  if (state === 'no-ref') return null;

  if (state === 'loading') {
    return (
      <section>
        <div
          className="rounded-lg animate-pulse mb-4"
          style={{ width: '140px', height: '28px', backgroundColor: 'rgb(var(--color-surface))' }}
        />
        <div className="rounded-xl animate-pulse mb-6" style={{ height: '120px', backgroundColor: 'rgb(var(--color-surface))' }} />
        <div className="flex flex-col gap-4">
          {[1, 2].map((i) => (
            <div
              key={i}
              className="rounded-xl animate-pulse"
              style={{ height: '80px', backgroundColor: 'rgb(var(--color-surface))' }}
            />
          ))}
        </div>
      </section>
    );
  }

  if (state === 'error') {
    return (
      <section>
        <h2 className="text-2xl font-bold mb-4" style={{ color: 'rgb(var(--color-magenta))' }}>
          Comments
        </h2>
        <p className="text-sm" style={{ color: 'rgb(var(--color-fg-muted))' }}>
          Comments couldn&apos;t be loaded.{' '}
          <button
            type="button"
            onClick={retry}
            className="hover:underline"
            style={{ color: 'rgb(var(--color-aqua))' }}
          >
            Try again
          </button>
        </p>
      </section>
    );
  }

  return (
    <section>
      <div className="flex items-center justify-between mb-6">
        <h2 className="text-2xl font-bold" style={{ color: 'rgb(var(--color-magenta))' }}>
          Comments
        </h2>
        <PlatformButtonGroup postUri={postUri!} />
      </div>

      {parentPost && <ParentPostCard post={parentPost} />}

      {replies.length === 0 && (
        <p className="text-center py-8" style={{ color: 'rgb(var(--color-fg-muted))' }}>
          No comments yet.
        </p>
      )}
      {replies.length > 0 && (
        <div className="flex flex-col gap-4">
          {replies.map((reply) => (
            <Comment key={reply.uri} reply={reply} />
          ))}
        </div>
      )}
    </section>
  );
}

function PlatformButtonGroup({ postUri }: { postUri: string }) {
  const [platform, setPlatform] = useState<PlatformKey>('bluesky');
  const [open, setOpen] = useState(false);
  const groupRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    try {
      const saved = localStorage.getItem('preferred-comment-platform');
      if (saved && PLATFORMS.some((p) => p.key === saved)) {
        setPlatform(saved as PlatformKey);
      }
    } catch {
      // localStorage unavailable
    }
  }, []);

  useEffect(() => {
    if (!open) return;
    const handleClick = (e: MouseEvent) => {
      if (!groupRef.current?.contains(e.target as Node)) setOpen(false);
    };
    const handleKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setOpen(false);
    };
    window.addEventListener('click', handleClick, true);
    window.addEventListener('keydown', handleKey);
    return () => {
      window.removeEventListener('click', handleClick, true);
      window.removeEventListener('keydown', handleKey);
    };
  }, [open]);

  const current = PLATFORMS.find((p) => p.key === platform)!;
  const url = platformPostUrl(postUri, current.domain);

  return (
    <div ref={groupRef} className="relative">
      <div
        className="inline-flex rounded-full overflow-hidden"
        style={{
          borderWidth: '1px',
          borderColor: 'rgb(var(--color-border))',
          backgroundColor: 'rgb(var(--color-surface))',
        }}
      >
        <a
          href={url}
          target="_blank"
          rel="noopener noreferrer"
          className="inline-flex items-center gap-2 px-4 py-2 text-sm font-medium transition-colors duration-200 hover:opacity-80"
          style={{ color: 'rgb(var(--color-fg-muted))' }}
        >
          <PlatformIcon platform={platform} />
          Join the conversation
        </a>
        <button
          type="button"
          onClick={() => setOpen(!open)}
          aria-expanded={open}
          aria-haspopup="true"
          aria-label="Select platform"
          className="flex items-center px-2.5 py-2 transition-colors duration-200 hover:opacity-80"
          style={{
            color: 'rgb(var(--color-fg-muted))',
            borderLeftWidth: '1px',
            borderColor: 'rgb(var(--color-border))',
          }}
        >
          <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
            <polyline points="6 9 12 15 18 9" />
          </svg>
        </button>
      </div>

      {open && (
        <div
          className="absolute right-0 top-full mt-2 rounded-xl py-1 min-w-[160px]"
          style={{
            backgroundColor: 'rgb(var(--color-surface))',
            borderWidth: '1px',
            borderColor: 'rgb(var(--color-border))',
            boxShadow: '0 4px 16px rgb(0 0 0 / 0.25)',
            zIndex: 30,
          }}
        >
          {PLATFORMS.map((p) => (
            <button
              key={p.key}
              type="button"
              onClick={() => {
                setPlatform(p.key);
                try { localStorage.setItem('preferred-comment-platform', p.key); } catch {}
                setOpen(false);
              }}
              className="flex w-full items-center gap-2.5 px-3.5 py-2 text-sm transition-colors duration-150"
              style={{
                color: p.key === platform
                  ? 'rgb(var(--color-aqua))'
                  : 'rgb(var(--color-fg-muted))',
                backgroundColor: p.key === platform
                  ? 'rgb(var(--color-aqua) / 0.08)'
                  : 'transparent',
              }}
            >
              <PlatformIcon platform={p.key} />
              <span className="flex-1 text-left">{p.name}</span>
              {p.key === platform && (
                <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
                  <polyline points="20 6 9 17 4 12" />
                </svg>
              )}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

const PLATFORM_FAVICONS: Record<PlatformKey, string> = {
  bluesky: 'https://bsky.app/static/favicon-32x32.png',
  blacksky: 'https://blacksky.app/favicon.ico',
  witchsky: 'https://witchsky.app/favicon.ico',
  mu: 'https://mu.social/favicon.ico',
};

function PlatformIcon({ platform }: { platform: PlatformKey }) {
  return (
    // eslint-disable-next-line @next/next/no-img-element
    <img
      src={PLATFORM_FAVICONS[platform]}
      alt=""
      width={14}
      height={14}
      className="rounded-sm"
      style={{ filter: 'brightness(0) invert(1)' }}
    />
  );
}

function ParentPostCard({ post }: { post: ParentPost }) {
  const displayName = post.author.displayName || post.author.handle;
  const date = new Date(post.record.createdAt);

  return (
    <div
      className="rounded-xl p-5 mb-6"
      style={{
        backgroundColor: 'rgb(var(--color-surface))',
        borderWidth: '1px',
        borderColor: 'rgb(var(--color-aqua) / 0.2)',
      }}
    >
      <div className="flex items-center gap-2 mb-3">
        {post.author.avatar && (
          <Image
            src={post.author.avatar}
            alt={displayName}
            width={28}
            height={28}
            className="rounded-full"
          />
        )}
        <div className="flex items-center gap-1.5 min-w-0">
          <a
            href={`https://bsky.app/profile/${post.author.handle}`}
            target="_blank"
            rel="noopener noreferrer"
            className="text-sm font-medium hover:underline truncate"
            style={{ color: 'rgb(var(--color-fg))' }}
          >
            {displayName}
          </a>
          <span className="text-xs shrink-0" style={{ color: 'rgb(var(--color-fg-muted))' }}>
            @{post.author.handle}
          </span>
        </div>
        <time
          dateTime={date.toISOString()}
          className="ml-auto text-xs shrink-0"
          style={{ color: 'rgb(var(--color-fg-muted))' }}
        >
          {date.toLocaleDateString('en-US', { year: 'numeric', month: 'short', day: 'numeric' })}
        </time>
      </div>
      <div className="text-sm mb-3" style={{ color: 'rgb(var(--color-fg))' }}>
        <RichText text={post.record.text} facets={post.record.facets} />
      </div>
      <div className="flex items-center gap-5 text-xs" style={{ color: 'rgb(var(--color-fg-muted))' }}>
        <span className="inline-flex items-center gap-1">
          <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
            <path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z" />
          </svg>
          {post.replyCount}
        </span>
        <span className="inline-flex items-center gap-1">
          <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
            <polyline points="17 1 21 5 17 9" />
            <path d="M3 11V9a4 4 0 0 1 4-4h14" />
            <polyline points="7 23 3 19 7 15" />
            <path d="M21 13v2a4 4 0 0 1-4 4H3" />
          </svg>
          {post.repostCount}
        </span>
        <span className="inline-flex items-center gap-1">
          <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
            <path d="M20.84 4.61a5.5 5.5 0 0 0-7.78 0L12 5.67l-1.06-1.06a5.5 5.5 0 0 0-7.78 7.78l1.06 1.06L12 21.23l7.78-7.78 1.06-1.06a5.5 5.5 0 0 0 0-7.78z" />
          </svg>
          {post.likeCount}
        </span>
      </div>
    </div>
  );
}

function Comment({ reply }: { reply: Reply }) {
  const displayName = reply.author.displayName || reply.author.handle;
  const date = new Date(reply.record.createdAt);

  return (
    <div
      className="rounded-xl p-4"
      style={{
        backgroundColor: 'rgb(var(--color-surface))',
        borderWidth: '1px',
        borderColor: 'rgb(var(--color-border))',
      }}
    >
      <div className="flex items-center gap-2 mb-2">
        {reply.author.avatar && (
          <Image
            src={reply.author.avatar}
            alt={displayName}
            width={24}
            height={24}
            className="rounded-full"
          />
        )}
        <a
          href={`https://bsky.app/profile/${reply.author.handle}`}
          target="_blank"
          rel="noopener noreferrer"
          className="text-sm font-medium hover:underline"
          style={{ color: 'rgb(var(--color-fg))' }}
        >
          {displayName}
        </a>
        <span className="text-xs" style={{ color: 'rgb(var(--color-fg-muted))' }}>
          @{reply.author.handle}
        </span>
        <time
          dateTime={date.toISOString()}
          className="ml-auto text-xs"
          style={{ color: 'rgb(var(--color-fg-muted))' }}
        >
          {date.toLocaleDateString('en-US', { year: 'numeric', month: 'short', day: 'numeric' })}
        </time>
      </div>
      <div className="text-sm" style={{ color: 'rgb(var(--color-fg))' }}>
        <RichText text={reply.record.text} facets={reply.record.facets} />
      </div>
    </div>
  );
}

function RichText({ text, facets }: { text: string; facets?: Facet[] }) {
  if (!facets || facets.length === 0) return <>{text}</>;

  const encoder = new TextEncoder();
  const decoder = new TextDecoder();
  const bytes = encoder.encode(text);

  const sorted = [...facets].sort((a, b) => a.index.byteStart - b.index.byteStart);
  const parts: React.ReactNode[] = [];
  let lastByte = 0;

  for (const facet of sorted) {
    if (facet.index.byteStart > lastByte) {
      parts.push(decoder.decode(bytes.slice(lastByte, facet.index.byteStart)));
    }

    const segment = decoder.decode(bytes.slice(facet.index.byteStart, facet.index.byteEnd));
    const link = facet.features.find((f) => f.$type === 'app.bsky.richtext.facet#link') as
      | Extract<FacetFeature, { $type: 'app.bsky.richtext.facet#link' }>
      | undefined;
    const mention = facet.features.find((f) => f.$type === 'app.bsky.richtext.facet#mention') as
      | Extract<FacetFeature, { $type: 'app.bsky.richtext.facet#mention' }>
      | undefined;

    if (link) {
      parts.push(
        <a
          key={facet.index.byteStart}
          href={link.uri}
          target="_blank"
          rel="noopener noreferrer"
          className="hover:underline"
          style={{ color: 'rgb(var(--color-aqua))' }}
        >
          {segment}
        </a>,
      );
    } else if (mention) {
      parts.push(
        <a
          key={facet.index.byteStart}
          href={`https://bsky.app/profile/${mention.did}`}
          target="_blank"
          rel="noopener noreferrer"
          className="hover:underline"
          style={{ color: 'rgb(var(--color-aqua))' }}
        >
          {segment}
        </a>,
      );
    } else {
      parts.push(segment);
    }

    lastByte = facet.index.byteEnd;
  }

  if (lastByte < bytes.length) {
    parts.push(decoder.decode(bytes.slice(lastByte)));
  }

  return <>{parts}</>;
}

async function resolveComments(atUri: string): Promise<{
  bskyPostRef: string | null;
  parentPost: ParentPost | null;
  replies: Reply[];
}> {
  const match = atUri.match(/^at:\/\/(did:[^/]+)\/([^/]+)\/(.+)$/);
  if (!match) throw new Error('Invalid AT URI');

  const [, did, collection, rkey] = match;

  const plcRes = await fetch(`https://plc.directory/${did}`);
  if (!plcRes.ok) throw new Error('DID resolution failed');
  const plcDoc = await plcRes.json();

  const pdsEndpoint = plcDoc.service?.find(
    (s: { id: string; type: string; serviceEndpoint: string }) => s.id === '#atproto_pds',
  )?.serviceEndpoint;
  if (!pdsEndpoint) throw new Error('No PDS endpoint');

  const recordRes = await fetch(
    `${pdsEndpoint}/xrpc/com.atproto.repo.getRecord?repo=${did}&collection=${collection}&rkey=${rkey}`,
  );
  if (!recordRes.ok) throw new Error('Record fetch failed');
  const recordData = await recordRes.json();

  const rawRef = recordData.value?.bskyPostRef;
  const bskyPostRef = typeof rawRef === 'string' ? rawRef : rawRef?.uri;
  if (typeof bskyPostRef !== 'string') return { bskyPostRef: null, parentPost: null, replies: [] };

  const threadRes = await fetch(
    `https://public.api.bsky.app/xrpc/app.bsky.feed.getPostThread?uri=${encodeURIComponent(bskyPostRef)}&depth=1`,
  );
  if (!threadRes.ok) return { bskyPostRef, parentPost: null, replies: [] };
  const threadData = await threadRes.json();

  const threadPost = threadData.thread?.post;
  const parentPost: ParentPost | null = threadPost
    ? {
        uri: threadPost.uri,
        author: threadPost.author,
        record: threadPost.record,
        likeCount: threadPost.likeCount ?? 0,
        repostCount: threadPost.repostCount ?? 0,
        replyCount: threadPost.replyCount ?? 0,
      }
    : null;

  const replies: Reply[] = (threadData.thread?.replies ?? [])
    .filter((r: { $type: string }) => r.$type === 'app.bsky.feed.defs#threadViewPost')
    .map((r: { post: { uri: string; author: Reply['author']; record: Reply['record']; likeCount?: number } }) => ({
      uri: r.post.uri,
      author: r.post.author,
      record: r.post.record,
      likeCount: r.post.likeCount,
    }))
    .sort(
      (a: Reply, b: Reply) =>
        new Date(a.record.createdAt).getTime() - new Date(b.record.createdAt).getTime(),
    );

  return { bskyPostRef, parentPost, replies };
}

function platformPostUrl(atUri: string, domain: string): string {
  const match = atUri.match(/^at:\/\/(did:[^/]+)\/[^/]+\/(.+)$/);
  if (!match) return `https://${domain}`;
  return `https://${domain}/profile/${match[1]}/post/${match[2]}`;
}
