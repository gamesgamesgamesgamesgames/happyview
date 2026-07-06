'use client';

import { useEffect } from 'react';

export function SequoiaCommentsLoader() {
  useEffect(() => {
    import('./sequoia-comments.js');
  }, []);

  return null;
}
