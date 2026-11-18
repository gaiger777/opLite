import "@/index.css";
import "@/i18n";

import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { App } from "@/App";
import { applyDensity, readDensity } from "@/lib/density";
import { applyTheme, readTheme } from "@/lib/theme";

applyTheme(readTheme());
applyDensity(readDensity());

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
