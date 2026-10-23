/// <reference types="vitest/config" />
import path from "node:path";

import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// 개발 서버는 /api(WebSocket 포함)를 opld로 넘긴다. opld의 인증서는 클러스터 CA가 서명했으므로
// 검증하지 않는다. Host를 바꾸지 않아야(changeOrigin 없음) 쿠키 요청의 Origin 검사를 통과한다.
const opld = process.env.OPLD_URL ?? "https://localhost:8006";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: { alias: { "@": path.resolve(import.meta.dirname, "src") } },
  // 관리 화면은 사내망에서 한 번 받아 캐시한다. noVNC는 따로 나눠 콘솔을 열 때만 받는다.
  build: { chunkSizeWarningLimit: 1024 },
  server: {
    proxy: { "/api": { target: opld, secure: false, ws: true } },
  },
  test: {
    environment: "jsdom",
    globals: true,
    setupFiles: ["src/test/setup.ts"],
    include: ["src/**/*.test.{ts,tsx}"],
  },
});
