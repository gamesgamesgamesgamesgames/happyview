import { createMDX } from "fumadocs-mdx/next";
import type { NextConfig } from "next";

const config: NextConfig = {
  transpilePackages: ["@happyview/design-system"],
};

const withMDX = createMDX();

export default withMDX(config);
