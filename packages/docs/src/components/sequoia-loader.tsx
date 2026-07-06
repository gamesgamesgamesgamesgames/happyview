'use client';

import { useEffect } from 'react';

export function SequoiaLoader() {
  useEffect(() => {
    import('./sequoia-comments.js');
  }, []);

  return null;
}
