import { Suspense } from "react";

import ScriptPerformance from "./script-performance";

export default function ScriptPerformancePage() {
  return (
    <Suspense>
      <ScriptPerformance />
    </Suspense>
  );
}
