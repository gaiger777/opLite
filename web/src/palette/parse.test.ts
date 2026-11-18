import { parseCommand } from "@/palette/parse";

it.each([
  ["start web", { verb: "start", target: "web" }],
  ["Start WEB", { verb: "start", target: "web" }],
  ["web 시작", { verb: "start", target: "web" }],
  ["web 켜기", { verb: "start", target: "web" }],
  ["stop web", { verb: "stop", target: "web" }],
  ["web을 끄기", { verb: "stop", target: "web" }],
  ["web 정지", { verb: "stop", target: "web" }],
  ["restart web", { verb: "reboot", target: "web" }],
  ["web 재시작", { verb: "reboot", target: "web" }],
  ["web 다시 시작", { verb: "reboot", target: "web" }],
  ["force stop web", { verb: "force-stop", target: "web" }],
  ["force-stop web", { verb: "force-stop", target: "web" }],
  ["web 강제 정지", { verb: "force-stop", target: "web" }],
  ["migrate web to node2", { verb: "migrate", target: "web", node: "node2" }],
  ["move web node2", { verb: "migrate", target: "web", node: "node2" }],
  ["migrate web", { verb: "migrate", target: "web" }],
  ["web을 node2로 옮기기", { verb: "migrate", target: "web", node: "node2" }],
  ["web node1으로 이동", { verb: "migrate", target: "web", node: "node1" }],
  ["snapshot web", { verb: "snapshot", target: "web" }],
  ["web 스냅샷", { verb: "snapshot", target: "web" }],
  ["console web", { verb: "console", target: "web" }],
  ["web 콘솔", { verb: "console", target: "web" }],
  ["open web", { verb: "open", target: "web" }],
  ["web 열기", { verb: "open", target: "web" }],
  // 노드는 옮기기에만 붙는다.
  ["start web node2", { verb: "start", target: "web" }],
])("parses %j", (input, command) => {
  expect(parseCommand(input)).toEqual(command);
});

it.each(["", "web", "start", "web server", "10.0.0.1"])("is not a command: %j", (input) => {
  expect(parseCommand(input)).toBeNull();
});
