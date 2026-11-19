import { render, screen } from "@testing-library/react";

import { ResourceHeader } from "@/components/ResourceHeader";

it("wraps a long meta path instead of scrolling sideways on phones", () => {
  // 폴더 경로는 이름 64자 × 깊이 16까지다. 휴대폰에서 가로로 넘치면 안 된다(리뷰 2).
  const path = `/${"platform-engineering-shared-services/".repeat(6)}leaf`;
  render(<ResourceHeader name="leaf" meta={path} />);
  const meta = screen.getByText(path);
  expect(meta).toHaveClass("break-all");
  expect(meta).toHaveClass("min-w-0");
});
