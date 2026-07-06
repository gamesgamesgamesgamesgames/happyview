import { BrowserOAuthClient } from "@atproto/oauth-client-browser";

let clientPromise: Promise<BrowserOAuthClient> | null = null;

export function getOAuthClient(): Promise<BrowserOAuthClient> {
  if (!clientPromise) {
    const origin = window.location.origin;
    const isLoopback =
      origin.startsWith("http://localhost") ||
      origin.startsWith("http://127.0.0.1");

    if (isLoopback) {
      const port = window.location.port;
      const redirectUri = `http://127.0.0.1:${port}/oauth/callback`;
      const clientId = `http://localhost?redirect_uri=${encodeURIComponent(redirectUri)}&scope=${encodeURIComponent("atproto repo:site.standard.graph.recommend repo:site.standard.graph.subscription")}`;

      clientPromise = BrowserOAuthClient.load({
        clientId,
        handleResolver: "https://bsky.social",
      });
    } else {
      clientPromise = BrowserOAuthClient.load({
        clientId: `${origin}/oauth-client-metadata.json`,
        handleResolver: "https://bsky.social",
      });
    }
  }

  return clientPromise;
}
