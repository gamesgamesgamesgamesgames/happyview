'use client';

import { useCallback, useEffect, useRef, useState } from 'react';
import { Agent } from '@atproto/api';
import { BrowserOAuthClient } from '@atproto/oauth-client-browser';
import { getOAuthClient } from '@/lib/atproto-oauth';

type OAuthSession = Awaited<ReturnType<BrowserOAuthClient['restore']>>;
type PendingAction = 'recommend' | 'subscribe';

interface EngagementActionsProps {
  documentUri?: string;
  publicationUri?: string;
}

export function EngagementActions({ documentUri, publicationUri }: EngagementActionsProps) {
  const clientRef = useRef<BrowserOAuthClient | null>(null);
  const [session, setSession] = useState<OAuthSession | null>(null);
  const [showLogin, setShowLogin] = useState(false);
  const [pendingAction, setPendingAction] = useState<PendingAction | null>(null);
  const [recommended, setRecommended] = useState(false);
  const [subscribed, setSubscribed] = useState(false);
  const [loading, setLoading] = useState<PendingAction | null>(null);

  useEffect(() => {
    let cancelled = false;

    (async () => {
      try {
        const client = await getOAuthClient();
        if (cancelled) return;
        clientRef.current = client;
        const result = await client.init();
        if (!cancelled && result?.session) {
          setSession(result.session);
        }
      } catch {
        // OAuth init can fail on localhost — use http://127.0.0.1:PORT instead
      }
    })();

    return () => { cancelled = true; };
  }, []);

  useEffect(() => {
    if (!session) return;

    const agent = new Agent(session);

    if (documentUri) {
      checkExistingRecord(agent, session.did, 'site.standard.graph.recommend', documentUri, 'document')
        .then(setRecommended);
    }

    if (publicationUri) {
      checkExistingRecord(agent, session.did, 'site.standard.graph.subscription', publicationUri, 'publication')
        .then(setSubscribed);
    }
  }, [session, documentUri, publicationUri]);

  const handleAction = useCallback((action: PendingAction) => {
    if (!session) {
      setPendingAction(action);
      setShowLogin(true);
      return;
    }

    performAction(session, action, documentUri, publicationUri, {
      setRecommended,
      setSubscribed,
      setLoading,
    });
  }, [session, documentUri, publicationUri]);

  useEffect(() => {
    if (session && pendingAction) {
      setPendingAction(null);
      handleAction(pendingAction);
    }
  }, [session, pendingAction, handleAction]);

  const handleLogin = async (handle: string) => {
    const client = clientRef.current;
    if (!client) return;

    setShowLogin(false);
    await client.signInRedirect(handle, {
      state: window.location.href,
    });
  };

  const handleLogout = async () => {
    if (!session || !clientRef.current) return;
    await clientRef.current.revoke(session.did);
    setSession(null);
    setRecommended(false);
    setSubscribed(false);
  };

  return (
    <div className="flex flex-wrap items-center gap-3">
      {documentUri && (
        <button
          type="button"
          onClick={() => handleAction('recommend')}
          disabled={loading === 'recommend'}
          className="inline-flex items-center gap-2 rounded-full px-4 py-2 text-sm font-medium transition-all duration-200"
          style={{
            backgroundColor: recommended
              ? 'rgb(var(--color-magenta) / 0.15)'
              : 'rgb(var(--color-surface))',
            color: recommended
              ? 'rgb(var(--color-magenta))'
              : 'rgb(var(--color-fg-muted))',
            borderWidth: '1px',
            borderColor: recommended
              ? 'rgb(var(--color-magenta) / 0.3)'
              : 'rgb(var(--color-border))',
          }}
        >
          <HeartIcon filled={recommended} />
          {loading === 'recommend' ? 'Saving...' : recommended ? 'Recommended' : 'Recommend'}
        </button>
      )}

      {publicationUri && (
        <button
          type="button"
          onClick={() => handleAction('subscribe')}
          disabled={loading === 'subscribe'}
          className="inline-flex items-center gap-2 rounded-full px-4 py-2 text-sm font-medium transition-all duration-200"
          style={{
            backgroundColor: subscribed
              ? 'rgb(var(--color-aqua) / 0.15)'
              : 'rgb(var(--color-surface))',
            color: subscribed
              ? 'rgb(var(--color-aqua))'
              : 'rgb(var(--color-fg-muted))',
            borderWidth: '1px',
            borderColor: subscribed
              ? 'rgb(var(--color-aqua) / 0.3)'
              : 'rgb(var(--color-border))',
          }}
        >
          <BellIcon filled={subscribed} />
          {loading === 'subscribe' ? 'Saving...' : subscribed ? 'Subscribed' : 'Subscribe'}
        </button>
      )}

      {session && (
        <button
          type="button"
          onClick={handleLogout}
          className="text-xs transition-colors duration-200 hover:underline"
          style={{ color: 'rgb(var(--color-fg-muted))' }}
        >
          Log out
        </button>
      )}

      {showLogin && (
        <LoginDialog
          onSubmit={handleLogin}
          onClose={() => {
            setShowLogin(false);
            setPendingAction(null);
          }}
        />
      )}
    </div>
  );
}

function LoginDialog({
  onSubmit,
  onClose,
}: {
  onSubmit: (handle: string) => void;
  onClose: () => void;
}) {
  const [handle, setHandle] = useState('');
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [onClose]);

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    const trimmed = handle.trim();
    if (trimmed) onSubmit(trimmed);
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center p-4"
      style={{ backgroundColor: 'rgb(0 0 0 / 0.6)' }}
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        className="w-full max-w-sm rounded-xl p-6"
        style={{
          backgroundColor: 'rgb(var(--color-surface))',
          borderWidth: '1px',
          borderColor: 'rgb(var(--color-border))',
        }}
      >
        <h2
          className="text-lg font-semibold mb-2"
          style={{ color: 'rgb(var(--color-fg))' }}
        >
          Log in with AT Protocol
        </h2>
        <p
          className="text-sm mb-4"
          style={{ color: 'rgb(var(--color-fg-muted))' }}
        >
          Enter your handle to continue.
        </p>
        <form onSubmit={handleSubmit} className="flex flex-col gap-3">
          <input
            ref={inputRef}
            type="text"
            value={handle}
            onChange={(e) => setHandle(e.target.value)}
            placeholder="yourname.bsky.social"
            className="rounded-lg px-3 py-2 text-sm outline-none"
            style={{
              backgroundColor: 'rgb(var(--color-bg))',
              color: 'rgb(var(--color-fg))',
              borderWidth: '1px',
              borderColor: 'rgb(var(--color-border))',
            }}
          />
          <div className="flex gap-2 justify-end">
            <button
              type="button"
              onClick={onClose}
              className="rounded-lg px-4 py-2 text-sm transition-colors duration-200"
              style={{ color: 'rgb(var(--color-fg-muted))' }}
            >
              Cancel
            </button>
            <button
              type="submit"
              disabled={!handle.trim()}
              className="rounded-lg px-4 py-2 text-sm font-medium transition-colors duration-200 disabled:opacity-50"
              style={{
                backgroundColor: 'rgb(var(--color-aqua))',
                color: 'rgb(var(--color-bg))',
              }}
            >
              Log in
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}

async function checkExistingRecord(
  agent: Agent,
  did: string,
  collection: string,
  targetUri: string,
  subjectField: string,
): Promise<boolean> {
  try {
    const { data } = await agent.com.atproto.repo.listRecords({
      repo: did,
      collection,
      limit: 100,
    });
    return data.records.some(
      (r) => (r.value as Record<string, unknown>)[subjectField] === targetUri,
    );
  } catch {
    return false;
  }
}

async function performAction(
  session: OAuthSession,
  action: PendingAction,
  documentUri: string | undefined,
  publicationUri: string | undefined,
  callbacks: {
    setRecommended: (v: boolean) => void;
    setSubscribed: (v: boolean) => void;
    setLoading: (v: PendingAction | null) => void;
  },
) {
  const agent = new Agent(session);
  callbacks.setLoading(action);

  try {
    if (action === 'recommend' && documentUri) {
      const existing = await findExistingRecord(
        agent, session.did, 'site.standard.graph.recommend', documentUri, 'document',
      );

      if (existing) {
        await agent.com.atproto.repo.deleteRecord({
          repo: session.did,
          collection: 'site.standard.graph.recommend',
          rkey: existing.split('/').pop()!,
        });
        callbacks.setRecommended(false);
      } else {
        await agent.com.atproto.repo.createRecord({
          repo: session.did,
          collection: 'site.standard.graph.recommend',
          record: {
            $type: 'site.standard.graph.recommend',
            document: documentUri,
            createdAt: new Date().toISOString(),
          },
        });
        callbacks.setRecommended(true);
      }
    }

    if (action === 'subscribe' && publicationUri) {
      const existing = await findExistingRecord(
        agent, session.did, 'site.standard.graph.subscription', publicationUri, 'publication',
      );

      if (existing) {
        await agent.com.atproto.repo.deleteRecord({
          repo: session.did,
          collection: 'site.standard.graph.subscription',
          rkey: existing.split('/').pop()!,
        });
        callbacks.setSubscribed(false);
      } else {
        await agent.com.atproto.repo.createRecord({
          repo: session.did,
          collection: 'site.standard.graph.subscription',
          record: {
            $type: 'site.standard.graph.subscription',
            publication: publicationUri,
            createdAt: new Date().toISOString(),
          },
        });
        callbacks.setSubscribed(true);
      }
    }
  } catch (err) {
    console.error(`Failed to ${action}:`, err);
  } finally {
    callbacks.setLoading(null);
  }
}

async function findExistingRecord(
  agent: Agent,
  did: string,
  collection: string,
  targetUri: string,
  subjectField: string,
): Promise<string | null> {
  try {
    const { data } = await agent.com.atproto.repo.listRecords({
      repo: did,
      collection,
      limit: 100,
    });
    const match = data.records.find(
      (r) => (r.value as Record<string, unknown>)[subjectField] === targetUri,
    );
    return match?.uri ?? null;
  } catch {
    return null;
  }
}

function HeartIcon({ filled }: { filled: boolean }) {
  return (
    <svg width="16" height="16" viewBox="0 0 24 24" fill={filled ? 'currentColor' : 'none'} stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <path d="M20.84 4.61a5.5 5.5 0 0 0-7.78 0L12 5.67l-1.06-1.06a5.5 5.5 0 0 0-7.78 7.78l1.06 1.06L12 21.23l7.78-7.78 1.06-1.06a5.5 5.5 0 0 0 0-7.78z" />
    </svg>
  );
}

function BellIcon({ filled }: { filled: boolean }) {
  return (
    <svg width="16" height="16" viewBox="0 0 24 24" fill={filled ? 'currentColor' : 'none'} stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <path d="M18 8A6 6 0 0 0 6 8c0 7-3 9-3 9h18s-3-2-3-9" />
      <path d="M13.73 21a2 2 0 0 1-3.46 0" />
    </svg>
  );
}
