import { NextRequest } from "next/server";

export function GET(request: NextRequest) {
  const origin = new URL(request.url).origin;

  return Response.json({
    client_id: `${origin}/oauth-client-metadata.json`,
    client_name: "HappyView",
    client_uri: origin,
    redirect_uris: [`${origin}/oauth/callback`],
    grant_types: ["authorization_code"],
    response_types: ["code"],
    scope:
      "atproto repo:site.standard.graph.recommend repo:site.standard.graph.subscription",
    token_endpoint_auth_method: "none",
    application_type: "web",
    dpop_bound_access_tokens: true,
  });
}
