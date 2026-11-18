import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { history, NODE, vm } from "@/test/fixtures";
import { mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";
import { VmDetailPage } from "@/vms/VmDetailPage";

const FIELDS = [
  "cpu",
  "mem_rss",
  "disk_read",
  "disk_write",
  "disk_read_iops",
  "disk_write_iops",
  "net_rx",
  "net_tx",
];

it("shows the VM's charts in the metrics tab after the summary", async () => {
  const web = vm("web", "running");
  const asked: { key: unknown; range: string | null }[] = [];
  mockLists({ vm: [web], node: [NODE] });
  server.use(
    http.get(apiUrl("/vms/:key/metrics/history"), ({ request, params }) => {
      asked.push({ key: params.key, range: new URL(request.url).searchParams.get("range") });
      return HttpResponse.json(history(FIELDS, { approx: true }));
    }),
  );
  const user = userEvent.setup();
  const { router } = renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web" });
  const tabs = await screen.findAllByRole("tab");
  expect(tabs.map((tab) => tab.textContent).slice(0, 3)).toEqual([
    "Summary",
    "Metrics",
    "Hardware",
  ]);
  // 탭을 열기 전에는 묻지 않는다.
  expect(asked).toEqual([]);
  await user.click(screen.getByRole("tab", { name: "Metrics" }));
  const panel = screen.getByRole("tabpanel", { name: "Metrics" });
  expect(
    await within(panel).findByRole("img", { name: /^Memory, last 1 hour: Memory \(RSS\) average/ }),
  ).toBeInTheDocument();
  expect(within(panel).getByRole("table", { name: "Disk summary" })).toHaveTextContent(
    "SeriesAVGMAXMINp50 ≈p95 ≈Read20 B/s40 B/s5 B/s20 B/s38 B/sWrite",
  );
  await user.click(within(panel).getByRole("radio", { name: "1 year" }));
  expect(await within(panel).findByRole("img", { name: /^CPU, last 1 year:/ })).toBeInTheDocument();
  expect(router.state.location.search).toBe("?tab=metrics&range=year");
  expect(asked).toEqual([
    { key: web.id, range: "hour" },
    { key: web.id, range: "year" },
  ]);
});
