import type { NextConfig } from "next";

const apiBase = process.env.API_URL || "http://localhost:3000";
const basePath = process.env.NEXT_PUBLIC_BASE_PATH || undefined;

const nextConfig: NextConfig = {
  reactCompiler: true,
  trailingSlash: true,
  images: { unoptimized: true },
  basePath,
};

if (process.env.NODE_ENV === "production") {
  nextConfig.output = "export";
} else {
  nextConfig.rewrites = async () => ({
    // beforeFiles rewrites run before the trailingSlash redirect,
    // preventing 308s on API fetch calls.
    beforeFiles: [
      { source: "/api/:path*", destination: `${apiBase}/api/:path*` },
      { source: "/admin/:path*", destination: `${apiBase}/admin/:path*` },
      { source: "/auth/:path*", destination: `${apiBase}/auth/:path*` },
      { source: "/xrpc/:path*", destination: `${apiBase}/xrpc/:path*` },
      { source: "/health", destination: `${apiBase}/health` },
      { source: "/health/", destination: `${apiBase}/health` },
      { source: "/config", destination: `${apiBase}/config` },
      { source: "/config/", destination: `${apiBase}/config` },
      { source: "/oauth/:path*", destination: `${apiBase}/oauth/:path*` },
      { source: "/external-auth/:path*", destination: `${apiBase}/external-auth/:path*` },
      { source: "/.well-known/:path*", destination: `${apiBase}/.well-known/:path*` },
    ],
    afterFiles: [],
    fallback: [],
  });
}

export default nextConfig;
