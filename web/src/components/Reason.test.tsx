import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useId } from "react";

import { ReasonButton, ReasonList, ReasonMenuItem } from "@/components/Reason";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";

// 메뉴 항목이 받은 typeahead 글자(textValue)를 적어 둔다. 항목은 그대로 그린다.
const textValues = vi.hoisted(() => [] as (string | undefined)[]);
vi.mock("@/components/ui/dropdown-menu", async (importOriginal) => {
  const ui = await importOriginal<typeof import("@/components/ui/dropdown-menu")>();
  return {
    ...ui,
    DropdownMenuItem: (props: React.ComponentProps<typeof ui.DropdownMenuItem>) => {
      textValues.push(props.textValue);
      return <ui.DropdownMenuItem {...props} />;
    },
  };
});

function Menu({ onStart, onStop }: { onStart(): void; onStop(): void }) {
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button>Actions</Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent>
        <ReasonMenuItem reason="vms.reason.powerPending" onSelect={onStart}>
          Start
        </ReasonMenuItem>
        <ReasonMenuItem onSelect={onStop}>Stop</ReasonMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

it("keeps a blocked menu item reachable and says why without running it", async () => {
  const onStart = vi.fn();
  const onStop = vi.fn();
  render(<Menu onStart={onStart} onStop={onStop} />);
  await userEvent.click(screen.getByRole("button", { name: "Actions" }));
  // 비활성 항목도 키보드로 닿는다.
  await userEvent.keyboard("{ArrowDown}");
  const start = screen.getByRole("menuitem", { name: "Start" });
  expect(start).toHaveFocus();
  expect(start).toHaveAttribute("aria-disabled", "true");
  expect(start).toHaveAccessibleDescription("A power action is still in progress.");
  // 골라도 동작하지 않고 메뉴도 닫히지 않는다.
  await userEvent.keyboard("{Enter}");
  await userEvent.click(start);
  expect(onStart).not.toHaveBeenCalled();
  expect(screen.getByRole("menuitem", { name: "Start" })).toBeInTheDocument();

  const stop = screen.getByRole("menuitem", { name: "Stop" });
  expect(stop).not.toHaveAttribute("aria-disabled");
  expect(stop).not.toHaveAccessibleDescription();
  await userEvent.click(stop);
  expect(onStop).toHaveBeenCalledOnce();
});

it("lets typeahead see only the name of a blocked menu item", async () => {
  render(<Menu onStart={() => {}} onStop={() => {}} />);
  textValues.length = 0;
  await userEvent.click(screen.getByRole("button", { name: "Actions" }));
  // 이유 글씨("A power action ...")가 붙은 항목도 이름만 쓴다.
  expect(new Set(textValues)).toEqual(new Set(["Start", "Stop"]));
});

function Buttons({ onStart, onStop }: { onStart(): void; onStop(): void }) {
  const base = useId();
  const reasons = ["vms.reason.stopped", "vms.reason.stopped", null];
  return (
    <form onSubmit={(e) => e.preventDefault()}>
      <ReasonButton reason={reasons[0]} reasons={base} onClick={onStop}>
        Stop
      </ReasonButton>
      <ReasonButton reason={reasons[1]} reasons={base} type="submit">
        Reboot
      </ReasonButton>
      <ReasonButton reason={reasons[2]} reasons={base} onClick={onStart}>
        Start
      </ReasonButton>
      <ReasonList base={base} reasons={reasons} />
    </form>
  );
}

it("keeps a blocked button in the tab order and says why without running it", async () => {
  const onStart = vi.fn();
  const onStop = vi.fn();
  render(<Buttons onStart={onStart} onStop={onStop} />);
  await userEvent.tab();
  const stop = screen.getByRole("button", { name: "Stop" });
  expect(stop).toHaveFocus();
  expect(stop).toHaveAttribute("aria-disabled", "true");
  expect(stop).toHaveAccessibleDescription("The VM is stopped.");
  await userEvent.keyboard("{Enter}");
  await userEvent.click(stop);
  expect(onStop).not.toHaveBeenCalled();

  // 폼 안의 막힌 제출 버튼은 폼을 보내지 않는다.
  const submitted = vi.fn((e: SubmitEvent) => e.preventDefault());
  document.querySelector("form")!.addEventListener("submit", submitted);
  await userEvent.click(screen.getByRole("button", { name: "Reboot" }));
  expect(submitted).not.toHaveBeenCalled();
  expect(screen.getByRole("button", { name: "Reboot" })).toHaveAccessibleDescription(
    "The VM is stopped.",
  );
  // 같은 이유는 한 번만 보인다.
  expect(screen.getAllByText("The VM is stopped.")).toHaveLength(1);

  const start = screen.getByRole("button", { name: "Start" });
  expect(start).not.toHaveAttribute("aria-disabled");
  expect(start).not.toHaveAccessibleDescription();
  await userEvent.click(start);
  expect(onStart).toHaveBeenCalledOnce();
});
