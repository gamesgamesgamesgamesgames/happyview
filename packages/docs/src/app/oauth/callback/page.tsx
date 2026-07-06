'use client';

import { useEffect, useState } from 'react';
import { getOAuthClient } from '@/lib/atproto-oauth';

export default function OAuthCallback() {
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    getOAuthClient()
      .then((client) => client.init())
      .then((result) => {
        const returnUrl = result?.state ?? '/blog';
        window.location.replace(returnUrl);
      })
      .catch((err) => {
        setError(err instanceof Error ? err.message : 'Login failed');
      });
  }, []);

  if (error) {
    return (
      <div className="mx-auto max-w-md px-6 py-24 text-center">
        <h1 className="text-2xl font-bold mb-4" style={{ color: 'rgb(var(--color-magenta))' }}>
          Login Failed
        </h1>
        <p className="text-sm mb-6" style={{ color: 'rgb(var(--color-fg-muted))' }}>
          {error}
        </p>
        <a
          href="/blog"
          className="text-sm hover:underline"
          style={{ color: 'rgb(var(--color-aqua))' }}
        >
          Back to blog
        </a>
      </div>
    );
  }

  return (
    <div className="mx-auto max-w-md px-6 py-24 text-center">
      <p style={{ color: 'rgb(var(--color-fg-muted))' }}>
        Completing login...
      </p>
    </div>
  );
}
