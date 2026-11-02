import { describe, expect, mock, test } from "bun:test";
import { WebcryptoKey } from "@atproto/jwk-webcrypto";
import { HappyViewSession } from "../session";

async function generateTestKey(): Promise<WebcryptoKey> {
  const keyPair = await crypto.subtle.generateKey(
    { name: "ECDSA", namedCurve: "P-256" },
    true,
    ["sign", "verify"],
  );
  return WebcryptoKey.fromKeypair(keyPair, crypto.randomUUID());
}

function createSession(overrides?: {
  instanceUrl?: string;
  dpopKey?: WebcryptoKey;
  fetchMock?: typeof globalThis.fetch;
}) {
  return async () => {
    const dpopKey = overrides?.dpopKey ?? (await generateTestKey());
    return new HappyViewSession({
      did: "did:plc:testuser",
      dpopKey,
      accessToken: "test-access-token",
      clientKey: "hvc_testkey",
      instanceUrl: overrides?.instanceUrl ?? "https://happyview.example.com",
      fetch: overrides?.fetchMock,
    });
  };
}

describe("HappyViewSession", () => {
  test("exposes did property", async () => {
    const session = await createSession()();
    expect(session.did).toBe("did:plc:testuser");
  });

  test("fetchHandler prepends instanceUrl to relative paths", async () => {
    let capturedUrl: string | undefined;
    const fetchMock = mock(async (input: RequestInfo | URL) => {
      capturedUrl = input instanceof URL ? input.toString() : String(input);
      return new Response(JSON.stringify({ ok: true }), { status: 200 });
    });

    const session = await createSession({ fetchMock })();
    await session.fetchHandler(
      "/xrpc/com.example.test.getStuff?param=value",
      {},
    );

    expect(capturedUrl).toBe(
      "https://happyview.example.com/xrpc/com.example.test.getStuff?param=value",
    );
  });

  test("fetchHandler sends an absolute URL on the instance as given", async () => {
    let capturedUrl: string | undefined;
    const fetchMock = mock(async (input: RequestInfo | URL) => {
      capturedUrl = input instanceof URL ? input.toString() : String(input);
      return new Response("{}", { status: 200 });
    });

    const session = await createSession({ fetchMock })();
    await session.fetchHandler(
      "https://happyview.example.com/xrpc/test.method",
      {},
    );

    expect(capturedUrl).toBe("https://happyview.example.com/xrpc/test.method");
  });

  test("fetchHandler sends no credentials anywhere but the instance", async () => {
    const fetchMock = mock(
      async () => new Response("{}", { status: 200 }),
    );
    const session = await createSession({ fetchMock })();

    for (const url of [
      "https://other-service.example.com/xrpc/test.method",
      "http://happyview.example.com/xrpc/test.method",
      "https://happyview.example.com:8443/xrpc/test.method",
      ".attacker.example/xrpc/test.method",
    ]) {
      await expect(session.fetchHandler(url, {})).rejects.toThrow(
        "only sends requests to https://happyview.example.com",
      );
    }
    expect(fetchMock).not.toHaveBeenCalled();
  });

  test("fetchHandler keeps requests under an instance's base path", async () => {
    const fetchMock = mock(
      async () => new Response("{}", { status: 200 }),
    );
    const session = await createSession({
      instanceUrl: "https://example.com/happyview",
      fetchMock,
    })();

    await session.fetchHandler("/xrpc/test.method", {});
    await expect(
      session.fetchHandler("https://example.com/other/xrpc/test.method", {}),
    ).rejects.toThrow();
    await expect(
      session.fetchHandler("https://example.com/happyviewer/xrpc/test.method", {}),
    ).rejects.toThrow();
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  test("a failed request's error carries neither the URL nor the fetch source", async () => {
    const fetchMock = mock(async () => {
      throw new Error("network down");
    });
    const session = await createSession({ fetchMock })();

    const error = await session
      .fetchHandler("/xrpc/test.method?secret=1", {})
      .catch((e: Error) => e);
    expect((error as Error).message).toBe("fetchHandler failed: network down");
    expect((error as Error).message).not.toContain("secret");
  });

  test("fetchHandler adds Authorization DPoP header", async () => {
    let capturedInit: RequestInit | undefined;
    const fetchMock = mock(async (_input: RequestInfo | URL, init?: RequestInit) => {
      capturedInit = init;
      return new Response("{}", { status: 200 });
    });

    const session = await createSession({ fetchMock })();
    await session.fetchHandler("/xrpc/test.method", {});

    const headers = new Headers(capturedInit?.headers);
    expect(headers.get("authorization")).toBe("DPoP test-access-token");
  });

  test("fetchHandler adds DPoP proof header as valid JWT", async () => {
    let capturedInit: RequestInit | undefined;
    const fetchMock = mock(async (_input: RequestInfo | URL, init?: RequestInit) => {
      capturedInit = init;
      return new Response("{}", { status: 200 });
    });

    const session = await createSession({ fetchMock })();
    await session.fetchHandler("/xrpc/test.method", {});

    const headers = new Headers(capturedInit?.headers);
    const dpopHeader = headers.get("dpop");
    expect(dpopHeader).not.toBeNull();
    expect(dpopHeader!.split(".")).toHaveLength(3);
  });

  test("fetchHandler adds X-Client-Key header", async () => {
    let capturedInit: RequestInit | undefined;
    const fetchMock = mock(async (_input: RequestInfo | URL, init?: RequestInit) => {
      capturedInit = init;
      return new Response("{}", { status: 200 });
    });

    const session = await createSession({ fetchMock })();
    await session.fetchHandler("/xrpc/test.method", {});

    const headers = new Headers(capturedInit?.headers);
    expect(headers.get("x-client-key")).toBe("hvc_testkey");
  });

  test("fetchHandler preserves existing headers from init", async () => {
    let capturedInit: RequestInit | undefined;
    const fetchMock = mock(async (_input: RequestInfo | URL, init?: RequestInit) => {
      capturedInit = init;
      return new Response("{}", { status: 200 });
    });

    const session = await createSession({ fetchMock })();
    await session.fetchHandler("/xrpc/test.method", {
      headers: { "content-type": "application/json" },
    });

    const headers = new Headers(capturedInit?.headers);
    expect(headers.get("content-type")).toBe("application/json");
    expect(headers.get("authorization")).toBe("DPoP test-access-token");
  });

  test("fetchHandler stores DPoP-Nonce from response", async () => {
    const fetchMock = mock(async () => {
      const headers = new Headers();
      headers.set("dpop-nonce", "server-nonce-abc");
      return new Response("{}", { status: 200, headers });
    });

    const session = await createSession({ fetchMock })();
    await session.fetchHandler("/xrpc/test.method", {});
    expect((session as any).dpopNonce).toBe("server-nonce-abc");
  });

  test("fetchHandler includes stored nonce in subsequent DPoP proofs", async () => {
    const capturedInits: RequestInit[] = [];
    let callCount = 0;
    const fetchMock = mock(async (_input: RequestInfo | URL, init?: RequestInit) => {
      capturedInits.push(init ?? {});
      callCount++;
      const headers = new Headers();
      if (callCount === 1) {
        headers.set("dpop-nonce", "server-nonce-xyz");
      }
      return new Response("{}", { status: 200, headers });
    });

    const session = await createSession({ fetchMock })();

    await session.fetchHandler("/xrpc/test.method", {});
    await session.fetchHandler("/xrpc/test.method2", {});

    function decodeJwtPayload(jwt: string): Record<string, unknown> {
      const payloadB64 = jwt.split(".")[1];
      const padded = payloadB64 + "=".repeat((4 - (payloadB64.length % 4)) % 4);
      const binary = atob(padded.replace(/-/g, "+").replace(/_/g, "/"));
      return JSON.parse(binary);
    }

    const firstDpop = new Headers(capturedInits[0].headers).get("dpop")!;
    const secondDpop = new Headers(capturedInits[1].headers).get("dpop")!;

    const firstPayload = decodeJwtPayload(firstDpop);
    const secondPayload = decodeJwtPayload(secondDpop);

    expect(firstPayload.nonce).toBeUndefined();
    expect(secondPayload.nonce).toBe("server-nonce-xyz");
  });
});
