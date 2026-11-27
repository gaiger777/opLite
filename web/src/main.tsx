import "@/index.css";

import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { App } from "@/App";
import { i18nReady } from "@/i18n";
import { onPreloadError } from "@/layout/RouteError";
import { applyDensity, readDensity } from "@/lib/density";
import { applyTheme, readTheme } from "@/lib/theme";

applyTheme(readTheme());
applyDensity(readDensity());
// 새로 배포해 옛 화면 조각이 없어졌다(설계 §9): 새로고침을 권한다.
window.addEventListener("vite:preloadError", onPreloadError);

// 고른 언어의 문구가 들어온 뒤 그린다(받지 못해도 그린다 — 새로 배포했으면 위 경고가 새로고침을 권한다).
void i18nReady
  .catch(() => {})
  .then(() =>
    createRoot(document.getElementById("root")!).render(
      <StrictMode>
        <App />
      </StrictMode>,
    ),
  );
