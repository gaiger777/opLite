import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { Inbox, TriangleAlert } from "lucide-react";

import type { Vm } from "@/api/types";
import { EmptyState } from "@/components/EmptyState";
import { ACTION_ICONS, AREA_ICONS } from "@/components/icons";
import { StatusChip } from "@/components/StatusChip";
import { Button } from "@/components/ui/button";
import { Kbd } from "@/components/ui/kbd";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
  SheetTrigger,
} from "@/components/ui/sheet";
import { Skeleton } from "@/components/ui/skeleton";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "@/components/ui/tooltip";
import { expectNoAxeViolations } from "@/test/axe";
import { PHASE_TONE, VmPhaseBadge } from "@/vms/VmPhaseBadge";

describe("StatusChip", () => {
  it("shows the text with a tone and a hidden dot", async () => {
    const { container } = render(<StatusChip tone="success">Running</StatusChip>);
    const chip = screen.getByText("Running");
    expect(chip).toHaveAttribute("data-tone", "success");
    expect(chip).toHaveClass("bg-success-soft", "text-success-fg");
    // 점은 꾸밈이라 읽지 않는다.
    expect(chip.querySelector("[aria-hidden]")).toHaveClass("bg-success");
    await expectNoAxeViolations(container, { disable: ["region"] });
  });

  it("can show an icon instead of the dot and pulse while busy", () => {
    const { rerender } = render(
      <StatusChip tone="warning" icon={TriangleAlert}>
        Degraded
      </StatusChip>,
    );
    expect(screen.getByText("Degraded").querySelector("svg")).toBeInTheDocument();
    rerender(
      <StatusChip tone="info" busy>
        Starting
      </StatusChip>,
    );
    expect(screen.getByText("Starting").querySelector("[aria-hidden]")).toHaveClass(
      "animate-pulse",
    );
  });
});

describe("VmPhaseBadge", () => {
  const vm = (phase: Vm["status"]["phase"]) => ({ status: { phase } }) as Vm;

  it.each(Object.entries(PHASE_TONE))("shows %s as a %s chip", (phase, tone) => {
    render(<VmPhaseBadge vm={vm(phase as Vm["status"]["phase"])} />);
    expect(screen.getByTestId("vm-phase")).toHaveAttribute("data-tone", tone);
  });

  it("keeps the translated phase text", () => {
    render(<VmPhaseBadge vm={vm("failed")} />);
    expect(screen.getByTestId("vm-phase")).toHaveTextContent("Failed");
  });
});

describe("EmptyState", () => {
  it("shows the description, the action and the guide link", async () => {
    const { container } = render(
      <EmptyState
        icon={Inbox}
        title="No storage pools"
        description="Create a pool to store disks."
        action={<Button>Create pool</Button>}
        docsHref="https://example.test/guide#pools"
      />,
    );
    expect(screen.getByText("No storage pools")).toBeInTheDocument();
    expect(screen.getByText("Create a pool to store disks.")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Create pool" })).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Read the guide" })).toHaveAttribute(
      "href",
      "https://example.test/guide#pools",
    );
    await expectNoAxeViolations(container, { disable: ["region"] });
  });

  it("leaves out what is not given", () => {
    render(<EmptyState title="Nothing here" />);
    expect(screen.queryByRole("link")).not.toBeInTheDocument();
    expect(screen.queryByRole("button")).not.toBeInTheDocument();
  });
});

describe("icons", () => {
  it("has one icon for each of the six areas", () => {
    expect(Object.keys(AREA_ICONS).sort()).toEqual(
      ["activity", "home", "infra", "protect", "settings", "vms"].sort(),
    );
    expect(new Set(Object.values(AREA_ICONS)).size).toBe(6);
  });

  it("does not reuse an action icon for another action", () => {
    expect(new Set(Object.values(ACTION_ICONS)).size).toBe(Object.keys(ACTION_ICONS).length);
  });
});

describe("ui primitives", () => {
  it("opens a sheet, traps focus and returns it on close", async () => {
    const user = userEvent.setup();
    render(
      <Sheet>
        <SheetTrigger asChild>
          <Button>Open center</Button>
        </SheetTrigger>
        <SheetContent>
          <SheetHeader>
            <SheetTitle>Notification center</SheetTitle>
            <SheetDescription>Running tasks and alerts</SheetDescription>
          </SheetHeader>
        </SheetContent>
      </Sheet>,
    );
    const trigger = screen.getByRole("button", { name: "Open center" });
    await user.click(trigger);
    const dialog = await screen.findByRole("dialog", { name: "Notification center" });
    await expectNoAxeViolations(dialog, { disable: ["region"] });
    await user.keyboard("{Escape}");
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    expect(trigger).toHaveFocus();
  });

  it("shows a tooltip on keyboard focus", async () => {
    const user = userEvent.setup();
    render(
      <TooltipProvider>
        <Tooltip>
          <TooltipTrigger asChild>
            <Button aria-label="Home">H</Button>
          </TooltipTrigger>
          <TooltipContent>Home</TooltipContent>
        </Tooltip>
      </TooltipProvider>,
    );
    await user.tab();
    expect(await screen.findByRole("tooltip")).toHaveTextContent("Home");
  });

  it("opens a popover", async () => {
    const user = userEvent.setup();
    render(
      <Popover>
        <PopoverTrigger asChild>
          <Button>Columns</Button>
        </PopoverTrigger>
        <PopoverContent>Pick columns</PopoverContent>
      </Popover>,
    );
    await user.click(screen.getByRole("button", { name: "Columns" }));
    expect(await screen.findByText("Pick columns")).toBeInTheDocument();
  });

  it("moves a toggle group with arrow keys", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    const { container } = render(
      <ToggleGroup type="single" aria-label="View" defaultValue="nodes" onValueChange={onChange}>
        <ToggleGroupItem value="nodes">Nodes</ToggleGroupItem>
        <ToggleGroupItem value="cluster">Cluster</ToggleGroupItem>
      </ToggleGroup>,
    );
    const nodes = screen.getByRole("radio", { name: "Nodes" });
    expect(nodes).toHaveAttribute("aria-checked", "true");
    await user.click(nodes);
    await user.keyboard("{ArrowRight}");
    expect(screen.getByRole("radio", { name: "Cluster" })).toHaveFocus();
    await user.keyboard(" ");
    expect(onChange).toHaveBeenLastCalledWith("cluster");
    await expectNoAxeViolations(container, { disable: ["region"] });
  });

  it("renders a skeleton and a key hint", () => {
    render(
      <>
        <Skeleton data-testid="loading" className="h-4 w-20" />
        <Kbd>⌘K</Kbd>
      </>,
    );
    expect(screen.getByTestId("loading")).toHaveClass("animate-pulse");
    expect(screen.getByText("⌘K").tagName).toBe("KBD");
  });
});

describe("expectNoAxeViolations", () => {
  it("fails on a button without a name", async () => {
    const { container } = render(
      <main>
        <button />
      </main>,
    );
    await expect(expectNoAxeViolations(container)).rejects.toThrow(/button-name/);
  });
});
