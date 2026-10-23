import js from "@eslint/js";
import reactHooks from "eslint-plugin-react-hooks";
import globals from "globals";
import tseslint from "typescript-eslint";

export default tseslint.config(
  { ignores: ["dist", "src/api/schema.d.ts", "playwright-report", "test-results"] },
  {
    files: ["**/*.{ts,tsx,js,mjs}"],
    extends: [js.configs.recommended, ...tseslint.configs.recommended],
    plugins: { "react-hooks": reactHooks },
    rules: {
      ...reactHooks.configs.recommended.rules,
      // React Compiler를 쓰지 않는다(TanStack Table의 훅은 컴파일러가 건너뛴다는 경고).
      "react-hooks/incompatible-library": "off",
      // 화면에 HTML을 직접 넣지 않는다(XSS). React의 이스케이프만 쓴다.
      "no-restricted-syntax": [
        "error",
        {
          selector: "JSXAttribute[name.name='dangerouslySetInnerHTML']",
          message: "Do not inject raw HTML.",
        },
        {
          selector: "Property[key.name='dangerouslySetInnerHTML']",
          message: "Do not inject raw HTML.",
        },
        {
          selector: "AssignmentExpression[left.property.name=/^(innerHTML|outerHTML)$/]",
          message: "Do not inject raw HTML.",
        },
        {
          selector: "CallExpression[callee.property.name='insertAdjacentHTML']",
          message: "Do not inject raw HTML.",
        },
      ],
    },
  },
  // 앱은 브라우저에서 돈다. E2E는 Node(Playwright)에서 돌며 환경 변수를 읽고, 브라우저 안의 코드도 담는다.
  { files: ["src/**"], languageOptions: { globals: globals.browser } },
  { files: ["e2e/**"], languageOptions: { globals: { ...globals.browser, ...globals.node } } },
  { files: ["*.config.{js,ts}", "scripts/**"], languageOptions: { globals: globals.node } },
);
