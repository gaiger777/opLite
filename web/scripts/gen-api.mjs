// API 문서(Rust 쪽 utoipa 정의)로 src/api/schema.d.ts를 만든다. API를 바꾸면 다시 실행한다.
import { execFileSync } from "node:child_process";
import { writeFileSync } from "node:fs";

import openapiTS, { astToString } from "openapi-typescript";

const doc = execFileSync("cargo", ["run", "-q", "-p", "opl-api", "--bin", "opl-api-openapi"], {
  encoding: "utf8",
  maxBuffer: 64 * 1024 * 1024,
  stdio: ["ignore", "pipe", "inherit"],
});
const ast = await openapiTS(JSON.parse(doc));
const banner = "// 생성된 파일이다. 고치지 말고 `pnpm gen:api`로 다시 만든다.\n\n";
writeFileSync(new URL("../src/api/schema.d.ts", import.meta.url), banner + astToString(ast));
