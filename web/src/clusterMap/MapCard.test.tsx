import { render, screen } from "@testing-library/react";

import { VmCard } from "@/clusterMap/MapCard";
import { mapVm } from "@/test/clusterMap";

it("shows the HA chip only for VMs the map treats as HA", () => {
  const name = () => "n1";
  const { rerender } = render(
    <VmCard
      vm={mapVm(1, "web", "n1", "running", { spec: { ha: true } })}
      nodeName={name}
      progress={null}
    />,
  );
  expect(screen.getByText("HA")).toBeInTheDocument();
  // `ignored`는 타일 이름·HA 레이어처럼 HA가 아니다
  rerender(
    <VmCard
      vm={mapVm(1, "web", "n1", "running", { spec: { ha: { state: "ignored" } as never } })}
      nodeName={name}
      progress={null}
    />,
  );
  expect(screen.queryByText("HA")).toBeNull();
});
