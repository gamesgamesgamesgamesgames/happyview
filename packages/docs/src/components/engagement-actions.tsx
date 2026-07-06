'use client';

import { useCallback, useEffect, useRef, useState } from 'react';
import { Agent } from '@atproto/api';
import { BrowserOAuthClient } from '@atproto/oauth-client-browser';
import { getOAuthClient } from '@/lib/atproto-oauth';

type OAuthSession = Awaited<ReturnType<BrowserOAuthClient['restore']>>;
type ActionType = 'recommend' | 'subscribe';

interface EngagementActionsProps {
  documentUri?: string;
  publicationUri?: string;
}

export function EngagementActions({ documentUri, publicationUri }: EngagementActionsProps) {
  const clientRef = useRef<BrowserOAuthClient | null>(null);
  const [session, setSession] = useState<OAuthSession | null>(null);
  const [ready, setReady] = useState(false);
  const [showLogin, setShowLogin] = useState(false);
  const [pendingAction, setPendingAction] = useState<ActionType | null>(null);
  const [recommended, setRecommended] = useState(false);
  const [subscribed, setSubscribed] = useState(false);
  const [loading, setLoading] = useState<ActionType | null>(null);
  const [actionError, setActionError] = useState<ActionType | null>(null);
  const [toast, setToast] = useState<string | null>(null);
  const toastTimerRef = useRef<ReturnType<typeof setTimeout>>(undefined);

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
      } finally {
        if (!cancelled) setReady(true);
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

  useEffect(() => {
    if (!actionError) return;
    const timer = setTimeout(() => setActionError(null), 3000);
    return () => clearTimeout(timer);
  }, [actionError]);

  const showToast = useCallback((message: string) => {
    clearTimeout(toastTimerRef.current);
    setToast(message);
    toastTimerRef.current = setTimeout(() => setToast(null), 3000);
  }, []);

  const handleAction = useCallback((action: ActionType) => {
    if (!session) {
      setPendingAction(action);
      setShowLogin(true);
      return;
    }

    performAction(session, action, documentUri, publicationUri, {
      setRecommended,
      setSubscribed,
      setLoading,
      setActionError,
      showToast,
    });
  }, [session, documentUri, publicationUri, showToast]);

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

  if (!ready) {
    return (
      <div className="flex flex-wrap items-center gap-3">
        {documentUri && <SkeletonPill />}
        {publicationUri && <SkeletonPill />}
      </div>
    );
  }

  const actionLabel = pendingAction === 'subscribe'
    ? 'subscribe for updates'
    : 'recommend this article';

  return (
    <div>
      <div className="flex flex-wrap items-center gap-3">
        {documentUri && (
          <button
            type="button"
            onClick={() => handleAction('recommend')}
            disabled={loading === 'recommend'}
            className="inline-flex items-center gap-2 rounded-full px-4 py-2 text-sm font-medium transition-all duration-200"
            style={{
              backgroundColor: actionError === 'recommend'
                ? 'rgb(var(--color-magenta) / 0.25)'
                : recommended
                  ? 'rgb(var(--color-magenta) / 0.15)'
                  : 'rgb(var(--color-surface))',
              color: actionError === 'recommend'
                ? 'rgb(var(--color-magenta))'
                : recommended
                  ? 'rgb(var(--color-magenta))'
                  : 'rgb(var(--color-fg-muted))',
              borderWidth: '1px',
              borderColor: actionError === 'recommend'
                ? 'rgb(var(--color-magenta) / 0.5)'
                : recommended
                  ? 'rgb(var(--color-magenta) / 0.3)'
                  : 'rgb(var(--color-border))',
            }}
          >
            <HeartIcon filled={recommended} />
            {actionError === 'recommend'
              ? 'Failed'
              : loading === 'recommend'
                ? 'Saving...'
                : recommended
                  ? 'Recommended'
                  : 'Recommend'}
          </button>
        )}

        {publicationUri && (
          <button
            type="button"
            onClick={() => handleAction('subscribe')}
            disabled={loading === 'subscribe'}
            className="inline-flex items-center gap-2 rounded-full px-4 py-2 text-sm font-medium transition-all duration-200"
            style={{
              backgroundColor: actionError === 'subscribe'
                ? 'rgb(var(--color-magenta) / 0.25)'
                : subscribed
                  ? 'rgb(var(--color-aqua) / 0.15)'
                  : 'rgb(var(--color-surface))',
              color: actionError === 'subscribe'
                ? 'rgb(var(--color-magenta))'
                : subscribed
                  ? 'rgb(var(--color-aqua))'
                  : 'rgb(var(--color-fg-muted))',
              borderWidth: '1px',
              borderColor: actionError === 'subscribe'
                ? 'rgb(var(--color-magenta) / 0.5)'
                : subscribed
                  ? 'rgb(var(--color-aqua) / 0.3)'
                  : 'rgb(var(--color-border))',
            }}
          >
            <BellIcon filled={subscribed} />
            {actionError === 'subscribe'
              ? 'Failed'
              : loading === 'subscribe'
                ? 'Saving...'
                : subscribed
                  ? 'Subscribed'
                  : 'Subscribe'}
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
      </div>

      <div
        className="grid transition-[grid-template-rows] duration-200 ease-out motion-reduce:!duration-0"
        style={{ gridTemplateRows: showLogin ? '1fr' : '0fr' }}
      >
        <div className="overflow-hidden">
          <InlineLogin
            action={actionLabel}
            onSubmit={handleLogin}
            onClose={() => { setShowLogin(false); setPendingAction(null); }}
            visible={showLogin}
          />
        </div>
      </div>

      {toast && (
        <div className="fixed bottom-6 left-0 right-0 flex justify-center z-40 pointer-events-none">
          <div
            className="pointer-events-auto rounded-full px-5 py-2.5 text-sm font-medium"
            style={{
              backgroundColor: 'rgb(var(--color-surface))',
              color: 'rgb(var(--color-fg))',
              borderWidth: '1px',
              borderColor: 'rgb(var(--color-border))',
              boxShadow: '0 4px 24px rgb(0 0 0 / 0.3)',
              animation: 'toast-in-out 3s ease-out forwards',
            }}
          >
            {toast}
          </div>
        </div>
      )}
    </div>
  );
}

function SkeletonPill() {
  return (
    <div
      className="rounded-full animate-pulse"
      style={{
        width: '120px',
        height: '36px',
        backgroundColor: 'rgb(var(--color-surface))',
      }}
    />
  );
}

function InlineLogin({
  action,
  onSubmit,
  onClose,
  visible,
}: {
  action: string;
  onSubmit: (handle: string) => void;
  onClose: () => void;
  visible: boolean;
}) {
  const [handle, setHandle] = useState('');
  const [validationError, setValidationError] = useState('');
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (visible) {
      const raf = requestAnimationFrame(() => inputRef.current?.focus());
      return () => cancelAnimationFrame(raf);
    }
  }, [visible]);

  useEffect(() => {
    if (!visible) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [visible, onClose]);

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    const trimmed = handle.trim();
    if (!trimmed) return;

    if (!trimmed.includes('.') && !trimmed.startsWith('did:')) {
      setValidationError('Enter a full handle (e.g. yourname.bsky.social) or a DID.');
      return;
    }

    setValidationError('');
    onSubmit(trimmed);
  };

  return (
    <div className="pt-4">
      <p className="text-sm mb-3" style={{ color: 'rgb(var(--color-fg-muted))' }}>
        Sign in with Bluesky to {action}.
      </p>
      <form onSubmit={handleSubmit} className="flex flex-wrap items-start gap-2">
        <div className="flex-1 min-w-[200px]">
          <input
            ref={inputRef}
            type="text"
            value={handle}
            onChange={(e) => {
              setHandle(e.target.value);
              if (validationError) setValidationError('');
            }}
            placeholder="yourname.bsky.social"
            tabIndex={visible ? 0 : -1}
            className="w-full rounded-lg px-3 py-2 text-sm outline-none"
            style={{
              backgroundColor: 'rgb(var(--color-bg))',
              color: 'rgb(var(--color-fg))',
              borderWidth: '1px',
              borderColor: validationError
                ? 'rgb(var(--color-magenta) / 0.5)'
                : 'rgb(var(--color-border))',
            }}
            aria-invalid={!!validationError}
            aria-describedby={validationError ? 'inline-handle-error' : undefined}
          />
          {validationError && (
            <p
              id="inline-handle-error"
              className="mt-1.5 text-xs"
              style={{ color: 'rgb(var(--color-magenta))' }}
            >
              {validationError}
            </p>
          )}
        </div>
        <button
          type="submit"
          disabled={!handle.trim()}
          tabIndex={visible ? 0 : -1}
          className="rounded-lg px-4 py-2 text-sm font-medium transition-colors duration-200 disabled:opacity-50"
          style={{
            backgroundColor: 'rgb(var(--color-aqua))',
            color: 'rgb(var(--color-bg))',
          }}
        >
          Log in
        </button>
        <button
          type="button"
          onClick={onClose}
          tabIndex={visible ? 0 : -1}
          className="rounded-lg px-4 py-2 text-sm transition-colors duration-200"
          style={{ color: 'rgb(var(--color-fg-muted))' }}
        >
          Cancel
        </button>
      </form>
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
  action: ActionType,
  documentUri: string | undefined,
  publicationUri: string | undefined,
  callbacks: {
    setRecommended: (v: boolean) => void;
    setSubscribed: (v: boolean) => void;
    setLoading: (v: ActionType | null) => void;
    setActionError: (v: ActionType | null) => void;
    showToast: (message: string) => void;
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
        callbacks.showToast('Recommendation removed');
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
        callbacks.showToast('Recommended!');
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
        callbacks.showToast('Unsubscribed');
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
        callbacks.showToast('Subscribed to updates');
      }
    }
  } catch (err) {
    console.error(`Failed to ${action}:`, err);
    callbacks.setActionError(action);
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
