import { act, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { EmptyState } from "@/components/EmptyState";
import { PageHeader } from "@/components/PageHeader";
import { Diagram } from "@/help/Diagram";
import { DIAGRAM_IDS } from "@/help/diagramIds";
import { closeHelp, openHelp } from "@/help/help";
import { HelpPanel } from "@/help/HelpPanel";
import i18n from "@/i18n";
import { expectNoAxeViolations } from "@/test/axe";

beforeAll(() => {
  i18n.addResourceBundle(
    "en",
    "translation",
    {
      helpInfo: {
        demo: {
          field: {
            what: "A demo field.",
            how: "It does things.",
            when: "Use it for tests.",
            caution: "Careful.",
            diagram: "haFencing",
          },
        },
      },
    },
    true,
    false,
  );
});
afterEach(() => {
  act(() => closeHelp());
  localStorage.clear();
});

it("shows the sections and the diagram of a topic", async () => {
  render(<HelpPanel />);
  act(() => openHelp({ topic: "demo.field", label: "Demo" }));
  const panel = await screen.findByRole("dialog", { name: "Demo" });
  expect(await within(panel).findByRole("heading", { name: "What is it" })).toBeInTheDocument();
  expect(panel).toHaveTextContent("It does things.");
  expect(panel).toHaveTextContent("Careful.");
  expect(await within(panel).findByRole("img", { name: /HA diagram/ })).toBeInTheDocument();
  await expectNoAxeViolations(panel);
});

it("draws every diagram with a name", async () => {
  render(
    <>
      {DIAGRAM_IDS.map((id) => (
        <Diagram key={id} id={id} />
      ))}
    </>,
  );
  const images = await screen.findAllByRole("img");
  expect(images).toHaveLength(DIAGRAM_IDS.length);
  for (const image of images) {
    expect(image.getAttribute("aria-label")).not.toMatch(/^helpInfo\./);
  }
});

it("remembers a closed page intro and keeps the ⓘ", async () => {
  const user = userEvent.setup();
  const { unmount } = render(
    <>
      <PageHeader title="Pools" help="demo" intro="Where VM disks live." />
      <HelpPanel />
    </>,
  );
  expect(screen.getByText("Where VM disks live.")).toBeVisible();
  await user.click(screen.getByRole("button", { name: /Hide the introduction/ }));
  expect(screen.queryByText("Where VM disks live.")).not.toBeInTheDocument();
  unmount();
  render(
    <>
      <PageHeader title="Pools" help="demo" intro="Where VM disks live." />
      <HelpPanel />
    </>,
  );
  expect(screen.queryByText("Where VM disks live.")).not.toBeInTheDocument();
  await user.click(screen.getByRole("button", { name: "Help: About this page" }));
  const panel = await screen.findByRole("dialog", { name: "About this page" });
  expect(await within(panel).findByText("Where VM disks live.")).toBeInTheDocument();
});

it("opens help from an empty state", async () => {
  const user = userEvent.setup();
  render(
    <>
      <EmptyState
        title="No pools yet."
        description="Add one."
        diagram="storageSharing"
        help="demo.field"
      />
      <HelpPanel />
    </>,
  );
  expect(await screen.findByRole("img", { name: /Storage diagram/ })).toBeInTheDocument();
  await user.click(screen.getByRole("button", { name: "Learn more" }));
  expect(
    await within(await screen.findByRole("dialog")).findByText("A demo field."),
  ).toBeInTheDocument();
});
