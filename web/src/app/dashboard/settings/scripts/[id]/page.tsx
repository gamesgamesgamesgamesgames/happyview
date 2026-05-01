import ScriptDetail from "./script-detail";

// https://github.com/vercel/next.js/issues/71862
// Returning [] fails with output:"export", so provide a dummy param.
export async function generateStaticParams() {
  return [{ id: "_" }];
}

export default function ScriptDetailPage() {
  return <ScriptDetail />;
}
