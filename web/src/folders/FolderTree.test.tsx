import { render, screen } from "@testing-library/react";

import { folderTree, ROOT_ID } from "@/folders/folders";
import { FolderTree } from "@/folders/FolderTree";
import { folder, folderId } from "@/test/fixtures";

it("expands the ancestors of a folder selected after the tree is shown", () => {
  // 접힌 /teamA 아래에 새로 만든 prod를 고르거나 앞·뒤로 가도 보이게 한다(리뷰 1).
  const tree = folderTree([folder("teamA"), folder("prod", "vm", "teamA")], "vm");
  const props = { tree, rootLabel: "Root", label: "Folders", onSelect: () => {} };
  const { rerender } = render(<FolderTree {...props} selected={ROOT_ID} />);
  expect(screen.getByRole("treeitem", { name: "teamA" })).toHaveAttribute("aria-expanded", "false");
  expect(screen.queryByRole("treeitem", { name: "prod" })).not.toBeInTheDocument();
  rerender(<FolderTree {...props} selected={folderId("prod")} />);
  expect(screen.getByRole("treeitem", { name: "prod" })).toHaveAttribute("aria-selected", "true");
  expect(screen.getByRole("treeitem", { name: "teamA" })).toHaveAttribute("aria-expanded", "true");
});
