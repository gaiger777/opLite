import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

/** 행 동작 이름("Delete backup web-1")에서 행 이름(web-1)을 꺼내는 꼴 */
const ROW_OF = [
  /^Delete backup (.+)$/,
  /^Copy (.+) to a remote cluster$/,
  /^Copy (.+) to another backup store$/,
  /^Move (.+) to a backup store$/,
  /^Verify (.+) now$/,
  /^Browse files of (.+)$/,
];

function rowOf(item: string): string {
  for (const re of ROW_OF) {
    const m = re.exec(item);
    if (m) {
      return m[1]!;
    }
  }
  throw new Error(`no row in ${item}`);
}

async function openMenu(item: string) {
  // 다른 행의 메뉴가 열려 있으면 닫는다.
  if (screen.queryByRole("menu")) {
    await userEvent.keyboard("{Escape}");
  }
  await userEvent.click(await screen.findByRole("button", { name: `Actions for ${rowOf(item)}` }));
}

/** 행의 ⋯ 메뉴(계획 77-6)를 열고 그 동작 항목을 돌려준다. 이름에서 행을 찾는다. */
export async function rowAction(item: string): Promise<HTMLElement> {
  await openMenu(item);
  return screen.findByRole("menuitem", { name: item });
}

/** 행의 ⋯ 메뉴에 그 동작이 없으면 null(메뉴가 아예 없어도 null). 연 메뉴는 닫는다. */
export async function rowActionOrNull(item: string): Promise<HTMLElement | null> {
  if (!screen.queryByRole("button", { name: `Actions for ${rowOf(item)}` })) {
    return null;
  }
  await openMenu(item);
  const found = screen.queryByRole("menuitem", { name: item });
  await userEvent.keyboard("{Escape}");
  return found;
}
