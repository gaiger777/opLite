import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import type { Principal } from "@/api/types";
import { AppLayout } from "@/layout/AppLayout";
import { expectNoAxeViolations } from "@/test/axe";
import { remote } from "@/test/fixtures";
import { ADMIN, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

const OPERATOR: Principal = {
  user: "00000000-0000-0000-0000-000000000002",
  name: "ops",
  role: "operator",
};
const VIEWER: Principal = { ...OPERATOR, name: "viewer", role: "viewer" };

beforeEach(() => {
  server.use(
    http.get(apiUrl("/cluster"), () =>
      HttpResponse.json({ name: "lab", ca_fingerprint: "x", node: "n1", members: [] }),
    ),
  );
});
afterEach(() => localStorage.clear());

function shell(url = "/", as = ADMIN) {
  return renderPage(<AppLayout />, { path: "*", url, as });
}

const where = (router: ReturnType<typeof shell>["router"]) =>
  `${router.state.location.pathname}${router.state.location.search}`;

describe("create menu", () => {
  it("lists what the role may create and opens the create dialog of that screen", async () => {
    const user = userEvent.setup();
    const { router } = shell();
    await user.click(await screen.findByRole("button", { name: "Create new" }));
    const menu = await screen.findByRole("menu");
    expect(
      within(menu)
        .getAllByRole("menuitem")
        .map((i) => i.textContent),
    ).toEqual([
      "Virtual machine",
      "VM import",
      "Storage pool",
      "Network",
      "Backup job",
      "ISO upload",
    ]);
    await user.click(within(menu).getByRole("menuitem", { name: "Storage pool" }));
    await waitFor(() => expect(where(router)).toBe("/storage-pools?create=1"));
  });

  it("gives operators only their kinds and viewers no button", async () => {
    const user = userEvent.setup();
    const { unmount } = shell("/", OPERATOR);
    await user.click(await screen.findByRole("button", { name: "Create new" }));
    expect(
      within(await screen.findByRole("menu"))
        .getAllByRole("menuitem")
        .map((i) => i.textContent),
    ).toEqual(["Virtual machine", "VM import", "Backup job", "ISO upload"]);
    unmount();
    shell("/", VIEWER);
    await screen.findByRole("button", { name: "Account (viewer)" });
    expect(screen.queryByRole("button", { name: "Create new" })).not.toBeInTheDocument();
  });
});

describe("cluster switch", () => {
  it("lists remote clusters only when opened and goes to one", async () => {
    let asked = 0;
    server.use(
      http.get(apiUrl("/remotes"), () => {
        asked += 1;
        return HttpResponse.json({ items: [remote("west"), remote("east")] });
      }),
    );
    const user = userEvent.setup();
    const { router } = shell("/vms");
    const button = await screen.findByRole("button", { name: "Switch cluster (now lab)" });
    expect(asked).toBe(0);
    await user.click(button);
    const menu = await screen.findByRole("menu");
    await within(menu).findByRole("menuitem", { name: "east" });
    expect(
      within(menu)
        .getAllByRole("menuitem")
        .map((i) => i.textContent),
    ).toEqual(["lab", "east", "west"]);
    await user.click(within(menu).getByRole("menuitem", { name: "west" }));
    await waitFor(() => expect(where(router)).toBe("/remotes/west"));
  });
});

describe("shortcuts", () => {
  it("goes to an area with g and a letter, but not while typing", async () => {
    localStorage.setItem("opl.nav.last.infra", "/volumes");
    const user = userEvent.setup();
    const { router } = shell();
    await screen.findByRole("navigation", { name: "Main" });
    await user.keyboard("gv");
    await waitFor(() => expect(where(router)).toBe("/vms"));
    await user.keyboard("gi");
    await waitFor(() => expect(where(router)).toBe("/volumes"));

    // 입력 칸 안의 글자는 단축키가 아니다.
    const input = document.createElement("input");
    document.body.append(input);
    input.focus();
    await user.keyboard("gh");
    expect(where(router)).toBe("/volumes");
    input.remove();
  });

  it("forgets g after a second", async () => {
    const now = vi.spyOn(Date, "now").mockReturnValue(1_000);
    const { router } = shell("/vms");
    await screen.findByRole("navigation", { name: "Main" });
    fireEvent.keyDown(document.body, { key: "g" });
    now.mockReturnValue(2_500);
    fireEvent.keyDown(document.body, { key: "h" });
    expect(where(router)).toBe("/vms");
    now.mockRestore();
  });

  it("opens the help with ? and from the user menu", async () => {
    const user = userEvent.setup();
    const { router } = shell();
    await screen.findByRole("navigation", { name: "Main" });
    await user.keyboard("?");
    const help = await screen.findByRole("dialog", { name: "Keyboard shortcuts" });
    expect(within(help).getByText("Command palette")).toBeInTheDocument();
    await expectNoAxeViolations(document.body);
    expect(within(help).getByText("Virtual machines").nextElementSibling).toHaveTextContent(
      "gthenv",
    );
    await user.keyboard("{Escape}");
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    await user.click(screen.getByRole("button", { name: "Account (admin)" }));
    await user.click(await screen.findByRole("menuitem", { name: "Keyboard shortcuts" }));
    expect(await screen.findByRole("dialog", { name: "Keyboard shortcuts" })).toBeInTheDocument();
    // 대화상자가 열려 있으면 한 글자 키를 받지 않는다.
    await user.keyboard("gv");
    expect(where(router)).toBe("/");
  });

  it("folds the rail with [ and opens the create menu with c", async () => {
    const user = userEvent.setup();
    const { router } = shell();
    await screen.findByRole("navigation", { name: "Main" });
    const before = screen.getByRole("button", { name: /sidebar$/ }).getAttribute("aria-label");
    await user.keyboard("[[");
    expect(screen.getByRole("button", { name: /sidebar$/ })).not.toHaveAttribute(
      "aria-label",
      before,
    );
    await user.keyboard("c");
    expect(await screen.findByRole("menu")).toBeInTheDocument();
    // 메뉴가 열려 있으면 g v는 이동하지 않는다.
    await user.keyboard("gv");
    expect(where(router)).toBe("/");
  });

  it("takes no keys on the console", async () => {
    const user = userEvent.setup();
    const { router } = shell("/vms/web/console");
    await screen.findByRole("navigation", { name: "Breadcrumb" });
    await user.keyboard("gh?");
    await user.keyboard("{Control>}k{/Control}");
    expect(where(router)).toBe("/vms/web/console");
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  it("focuses the list search with / and presses the favorite star with f", async () => {
    const user = userEvent.setup();
    shell("/vms");
    await screen.findByRole("navigation", { name: "Main" });
    // 목록 화면과 상세 머리가 다는 표시(본문 안에서만 찾는다)
    const main = document.getElementById("main")!;
    const box = document.createElement("input");
    box.setAttribute("data-list-search", "");
    const star = document.createElement("button");
    star.setAttribute("data-pin-toggle", "");
    const pressed = vi.fn();
    star.addEventListener("click", pressed);
    main.append(box, star);
    await user.keyboard("f");
    expect(pressed).toHaveBeenCalledTimes(1);
    await user.keyboard("/");
    expect(box).toHaveFocus();
    // 검색 칸 안의 f는 글자다.
    await user.keyboard("f");
    expect(pressed).toHaveBeenCalledTimes(1);
    expect(box).toHaveValue("f");
  });
});
