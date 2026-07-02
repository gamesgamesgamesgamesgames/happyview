import { execSync } from 'node:child_process';
import { readFileSync } from 'node:fs';

const config = JSON.parse(readFileSync(new URL('../sequoia.json', import.meta.url), 'utf-8'));

if (!config.publicationUri) {
  console.log('[sequoia] No publicationUri configured — run `sequoia init` to set up. Skipping publish.');
  process.exit(0);
}

if (!process.env.ATP_IDENTIFIER && !process.env.ATP_APP_PASSWORD) {
  console.log('[sequoia] No ATP_IDENTIFIER/ATP_APP_PASSWORD set — skipping publish.');
  process.exit(0);
}

try {
  execSync('sequoia publish', { stdio: 'inherit', cwd: new URL('..', import.meta.url).pathname });
} catch (err) {
  console.error('[sequoia] Publish failed:', err.message);
  console.error('[sequoia] Continuing build without publishing.');
}
