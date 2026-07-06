import { createMDX } from "fumadocs-mdx/next";
import type { NextConfig } from "next";

const config: NextConfig = {
  allowedDevOrigins: ["127.0.0.1"],
  transpilePackages: ["@happyview/design-system"],
};

const withMDX = createMDX();

export default withMDX(config);
