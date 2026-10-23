// API 문서(Rust 쪽 utoipa 정의)로 src/api/schema.d.ts를 만든다. API를 바꾸면 다시 실행한다.
import { execFileSync } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import openapiTS, { astToString } from "openapi-typescript";

// 어느 디렉터리에서 실행해도 저장소 루트에서 cargo를 돌린다. Cargo.lock은 고치지 않는다.
const doc = execFileSync(
  "cargo",
  ["run", "-q", "--locked", "-p", "opl-api", "--bin", "opl-api-openapi"],
  {
    cwd: fileURLToPath(new URL("../../", import.meta.url)),
    encoding: "utf8",
    maxBuffer: 64 * 1024 * 1024,
    stdio: ["ignore", "pipe", "inherit"],
  },
);
const ast = await openapiTS(JSON.parse(doc));
const banner = "// 생성된 파일이다. 고치지 말고 `pnpm gen:api`로 다시 만든다.\n\n";
const out = new URL("../src/api/", import.meta.url);
mkdirSync(out, { recursive: true });
writeFileSync(new URL("schema.d.ts", out), banner + astToString(ast));
