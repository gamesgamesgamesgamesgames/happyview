import { Suspense } from "react";

import CataloguePlugin from "./catalogue-plugin";

// https://github.com/vercel/next.js/issues/71862
// Returning [] fails with output:"export", so provide a dummy param.
export async function generateStaticParams() {
  return [{ id: "_" }];
}

export default function CataloguePluginPage() {
  return (
    <Suspense>
      <CataloguePlugin />
    </Suspense>
  );
}
