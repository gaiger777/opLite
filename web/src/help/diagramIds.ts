// 도움말 그림의 이름(계획 77-4). 그림 자체(`help/diagrams`)는 늦게 받는다 — 여기는 이름만 둔다.
export const DIAGRAM_IDS = [
  "haFencing",
  "backupChain",
  "replicationDr",
  "sdn",
  "securityGroup",
  "placement",
  "storageSharing",
  "thinOvercommit",
  "liveMigration",
  "declarativeApply",
  "dedupStore",
  "quorum",
] as const;

export type DiagramId = (typeof DIAGRAM_IDS)[number];
