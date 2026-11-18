import axe from "axe-core";

/**
 * `axe-core`로 접근성 위반이 없는지 본다(설계 §10). jsdom은 색을 계산하지 못하므로 `color-contrast`는 끄고
 * `tokens.test.ts`가 대신한다. 화면 일부만 그린 시험이면 `region`(모든 내용이 랜드마크 안에) 같은 규칙을
 * `disable`로 끈다.
 */
export async function expectNoAxeViolations(
  container: Element,
  { disable = [] }: { disable?: string[] } = {},
) {
  const rules: Record<string, { enabled: boolean }> = { "color-contrast": { enabled: false } };
  for (const id of disable) {
    rules[id] = { enabled: false };
  }
  const { violations } = await axe.run(container, { rules, resultTypes: ["violations"] });
  // 실패하면 규칙과 걸린 요소가 보이게 줄여서 비교한다.
  expect(
    violations.map((v) => ({ id: v.id, help: v.help, nodes: v.nodes.map((n) => n.html) })),
  ).toEqual([]);
}
