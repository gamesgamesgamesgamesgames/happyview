#!/usr/bin/env node

const REPO = "gamesgamesgamesgamesgames/happyview";
const API_BASE = "https://api.github.com";

const CHANGELOGS = [
  {
    name: "HappyView",
    output: "content/docs/reference/changelog.md",
    match: (tag) => /^v\d/.test(tag),
    formatVersion: (tag) => tag,
  },
  {
    name: "@happyview/oauth-client",
    output: "content/docs/sdk/oauth-client/changelog.md",
    match: (tag) => tag.startsWith("@happyview/oauth-client-v"),
    formatVersion: (tag) => tag.replace("@happyview/oauth-client-", ""),
  },
  {
    name: "@happyview/oauth-client-browser",
    output: "content/docs/sdk/oauth-client-browser/changelog.md",
    match: (tag) => tag.startsWith("@happyview/oauth-client-browser-v"),
    formatVersion: (tag) => tag.replace("@happyview/oauth-client-browser-", ""),
  },
  {
    name: "@happyview/oauth-client-node",
    output: "content/docs/sdk/oauth-client-node/changelog.md",
    match: (tag) => tag.startsWith("@happyview/oauth-client-node-v"),
    formatVersion: (tag) => tag.replace("@happyview/oauth-client-node-", ""),
  },
  {
    name: "@happyview/lex-agent",
    output: "content/docs/sdk/lex-agent/changelog.md",
    match: (tag) => tag.startsWith("@happyview/lex-agent-v"),
    formatVersion: (tag) => tag.replace("@happyview/lex-agent-", ""),
  },
];

const token = process.env.GITHUB_TOKEN || process.env.GH_TOKEN;

async function fetchAllReleases() {
  const releases = [];
  let page = 1;

  while (true) {
    const url = `${API_BASE}/repos/${REPO}/releases?per_page=100&page=${page}`;
    const headers = { Accept: "application/vnd.github+json" };
    if (token) headers.Authorization = `Bearer ${token}`;

    const res = await fetch(url, { headers });

    if (!res.ok) {
      if (res.status === 403 && !token) {
        console.warn(
          "GitHub API rate limit hit. Set GITHUB_TOKEN or GH_TOKEN to authenticate."
        );
        return null;
      }
      throw new Error(`GitHub API error: ${res.status} ${res.statusText}`);
    }

    const batch = await res.json();
    if (batch.length === 0) break;
    releases.push(...batch);
    page++;
  }

  return releases;
}

function cleanReleaseBody(body) {
  if (!body) return "";
  let cleaned = body.trim();
  cleaned = cleaned.replace(/^#{1,3}\s+.*?\d+\.\d+\.\d+.*\n*/m, "");
  cleaned = cleaned.trim();
  return cleaned;
}

function buildMarkdown(changelog, releases) {
  const matching = releases
    .filter((r) => !r.prerelease && changelog.match(r.tag_name))
    .sort((a, b) => new Date(b.published_at) - new Date(a.published_at));

  if (matching.length === 0) return null;

  const lines = [
    `---`,
    `title: "${changelog.name}"`,
    `---`,
    "",
    `<!-- Generated automatically from GitHub releases. Do not edit by hand. -->`,
    "",
  ];

  for (const release of matching) {
    const version = changelog.formatVersion(release.tag_name);
    const date = release.published_at.slice(0, 10);
    const title = release.name && release.name !== release.tag_name
      ? `${version} — ${release.name}`
      : version;

    lines.push(`## ${title}`);
    lines.push("");
    lines.push(`*Released ${date}*`);
    lines.push("");

    const body = cleanReleaseBody(release.body);
    if (body) {
      lines.push(body);
    }

    lines.push("");
  }

  return lines.join("\n");
}

import { writeFileSync, readFileSync, existsSync } from "node:fs";
import { resolve, dirname } from "node:path";
import { mkdirSync } from "node:fs";

const ROOT = dirname(new URL(import.meta.url).pathname).replace(
  /\/scripts$/,
  ""
);

async function main() {
  console.log("Fetching releases from GitHub...");
  const releases = await fetchAllReleases();

  if (releases === null) {
    console.warn("Skipping changelog generation (no API access). Using existing files.");
    return;
  }

  console.log(`Found ${releases.length} total releases.`);

  const generated = [];

  for (const changelog of CHANGELOGS) {
    let md = buildMarkdown(changelog, releases);
    if (!md) {
      md = [
        `---`,
        `title: "${changelog.name}"`,
        `---`,
        "",
        `<!-- Generated automatically from GitHub releases. Do not edit by hand. -->`,
        "",
        "No releases yet.",
        "",
      ].join("\n");
      console.log(`  ${changelog.name}: no releases found, wrote placeholder.`);
    } else {
      console.log(`  ${changelog.name}: wrote ${changelog.output}`);
    }

    const outPath = resolve(ROOT, changelog.output);
    mkdirSync(dirname(outPath), { recursive: true });
    writeFileSync(outPath, md);
    generated.push(changelog);
  }

  console.log(`Generated ${generated.length} changelog(s).`);
}

// A production build must not ship stale changelogs. Anywhere else, such as a
// pull request preview without a token or one rate limited on a shared build
// host, the changelogs committed to the repo are good enough.
const production = process.env.VERCEL_ENV === "production";

main().catch((err) => {
  if (production) {
    console.error("Changelog generation failed:", err.message);
    process.exit(1);
  }
  console.warn(
    `Changelog generation failed (${err.message}). Using existing files.`
  );
});
