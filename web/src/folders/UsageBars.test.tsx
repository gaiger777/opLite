import { render, screen } from "@testing-library/react";

import { UsageBars, usageBars } from "@/folders/UsageBars";

const A = "019a0f3c-0000-7000-8009-00000000000a";
const P = "019a0f3c-0000-7000-8009-00000000000b";

const view = {
  folder: A,
  path: "/teamA/prod",
  usage: { vcpus: 4, memory_mib: 4096, disk_bytes: 0, vms: 2 },
  limits: [
    {
      folder: A,
      path: "/teamA/prod",
      quota: { vcpus: 64 },
      usage: { vcpus: 4, memory_mib: 4096, disk_bytes: 0, vms: 2 },
    },
    {
      folder: P,
      path: "/teamA",
      quota: { vcpus: 8, vms: 2 },
      usage: { vcpus: 6, memory_mib: 6144, disk_bytes: 0, vms: 3 },
    },
  ],
  // 자식 상한(64)이 부모(8)보다 커서 부모가 묶는다.
  binding: { vcpus: P, vms: P },
};

it("shows the binding folder's usage and limit, and the parent that binds", () => {
  expect(usageBars(view)).toEqual([
    { dim: "vcpus", used: 6, limit: 8, boundBy: "/teamA" },
    { dim: "memory_mib", used: 4096 },
    { dim: "disk_bytes", used: 0 },
    { dim: "vms", used: 3, limit: 2, boundBy: "/teamA" },
  ]);
  render(<UsageBars view={view} />);
  expect(screen.getByRole("meter", { name: "vCPUs" })).toHaveAttribute("aria-valuenow", "6");
  expect(screen.getAllByText("Limited by /teamA")).toHaveLength(2);
  // VM 수는 상한을 넘었다(상한을 사용량 밑으로 낮췄다).
  expect(screen.getByText("Over the limit")).toBeInTheDocument();
  expect(screen.getAllByText("No limit")).toHaveLength(2);
});
