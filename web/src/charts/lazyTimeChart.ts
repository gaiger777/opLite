import { lazy } from "react";

// uPlot은 차트를 그릴 때만 내려받는다.
export const TimeChart = lazy(() => import("@/charts/TimeChart"));
