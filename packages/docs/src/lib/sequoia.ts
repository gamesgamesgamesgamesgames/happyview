import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

let cachedUri: string | undefined;
let loaded = false;

export function getSequoiaPublicationUri(): string | undefined {
  if (loaded) return cachedUri;
  loaded = true;

  try {
    const configPath = resolve(process.cwd(), 'sequoia.json');
    const config = JSON.parse(readFileSync(configPath, 'utf-8'));
    cachedUri = config.publicationUri || undefined;
  } catch {
    cachedUri = undefined;
  }

  return cachedUri;
}
