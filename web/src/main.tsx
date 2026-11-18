import "@/index.css";
import "@/i18n";

import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { App } from "@/App";
import { onPreloadError } from "@/layout/RouteError";
import { applyDensity, readDensity } from "@/lib/density";
import { applyTheme, readTheme } from "@/lib/theme";

applyTheme(readTheme());
applyDensity(readDensity());
// 새로 배포해 옛 화면 조각이 없어졌다(설계 §9): 새로고침을 권한다.
window.addEventListener("vite:preloadError", onPreloadError);

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
