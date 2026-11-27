// 도움말 그림(계획 77-4, 진단 §2.0·§2.7~§2.10): 손으로 그린 작은 SVG 도식. 색은 테마 토큰(밝게·어둡게 함께),
// 글은 늦게 받는 문구 `helpInfo.diagram.<그림>.*`. 도움말 패널과 빈 상태가 늦게 읽는다(`help/Diagram.tsx`).
import { useTranslation } from "react-i18next";

import type { DiagramId } from "@/help/diagramIds";

type Tone = "plain" | "brand" | "ok" | "bad" | "warn" | "muted";

const STROKE: Record<Tone, string> = {
  plain: "var(--border)",
  brand: "var(--brand)",
  ok: "var(--success)",
  bad: "var(--danger)",
  warn: "var(--warning)",
  muted: "var(--muted-foreground)",
};
const FILL: Record<Tone, string> = {
  plain: "var(--card)",
  brand: "var(--info-soft)",
  ok: "var(--success-soft)",
  bad: "var(--danger-soft)",
  warn: "var(--warning-soft)",
  muted: "var(--muted)",
};

/** 글 한 줄 */
function L({
  x,
  y,
  children,
  tone,
  anchor = "middle",
  size = 11,
  bold,
}: {
  x: number;
  y: number;
  children: React.ReactNode;
  tone?: Tone;
  anchor?: "start" | "middle" | "end";
  size?: number;
  bold?: boolean;
}) {
  return (
    <text
      x={x}
      y={y}
      textAnchor={anchor}
      dominantBaseline="middle"
      fontSize={size}
      fontWeight={bold ? 600 : 400}
      fill={
        tone === "muted"
          ? "var(--muted-foreground)"
          : tone && tone !== "plain"
            ? STROKE[tone]
            : "var(--foreground)"
      }
    >
      {children}
    </text>
  );
}

/** 둥근 상자(가운데 글은 `label`) */
function Box({
  x,
  y,
  w,
  h,
  tone = "plain",
  label,
  dashed,
  labelY,
}: {
  x: number;
  y: number;
  w: number;
  h: number;
  tone?: Tone;
  label?: React.ReactNode;
  dashed?: boolean;
  /** 글의 세로 위치(기본은 가운데) */
  labelY?: number;
}) {
  return (
    <g>
      <rect
        x={x}
        y={y}
        width={w}
        height={h}
        rx={6}
        fill={FILL[tone]}
        stroke={STROKE[tone === "plain" ? "muted" : tone]}
        strokeWidth={1.25}
        strokeDasharray={dashed ? "4 3" : undefined}
      />
      {label !== undefined && (
        <L x={x + w / 2} y={labelY ?? y + h / 2} size={10.5}>
          {label}
        </L>
      )}
    </g>
  );
}

/** 화살표(끝에 머리). `label`은 선 가운데 위에 */
function Arrow({
  x1,
  y1,
  x2,
  y2,
  tone = "muted",
  label,
  dashed,
  labelDy = -8,
}: {
  x1: number;
  y1: number;
  x2: number;
  y2: number;
  tone?: Tone;
  label?: React.ReactNode;
  dashed?: boolean;
  labelDy?: number;
}) {
  const angle = Math.atan2(y2 - y1, x2 - x1);
  const head = (da: number) => `${x2 - 7 * Math.cos(angle + da)},${y2 - 7 * Math.sin(angle + da)}`;
  return (
    <g>
      <line
        x1={x1}
        y1={y1}
        x2={x2}
        y2={y2}
        stroke={STROKE[tone]}
        strokeWidth={1.5}
        strokeDasharray={dashed ? "4 3" : undefined}
      />
      <polygon points={`${x2},${y2} ${head(0.45)} ${head(-0.45)}`} fill={STROKE[tone]} />
      {label !== undefined && (
        <L x={(x1 + x2) / 2} y={(y1 + y2) / 2 + labelDy} size={10} tone={tone}>
          {label}
        </L>
      )}
    </g>
  );
}

/** 단계 번호 동그라미 */
function Step({ x, y, n }: { x: number; y: number; n: number }) {
  return (
    <g>
      <circle cx={x} cy={y} r={8} fill="var(--brand)" />
      <L x={x} y={y + 0.5} size={10} bold>
        <tspan fill="var(--brand-foreground)">{n}</tspan>
      </L>
    </g>
  );
}

/** 작은 VM 칸 */
function Vm({
  x,
  y,
  tone = "brand",
  label = "VM",
}: {
  x: number;
  y: number;
  tone?: Tone;
  label?: string;
}) {
  return <Box x={x} y={y} w={34} h={18} tone={tone} label={label} />;
}

function Frame({
  id,
  h = 170,
  children,
}: {
  id: DiagramId;
  h?: number;
  children: React.ReactNode;
}) {
  const { t } = useTranslation();
  return (
    <svg
      data-diagram={id}
      viewBox={`0 0 320 ${h}`}
      role="img"
      aria-label={t(`helpInfo.diagram.${id}.alt`)}
      className="h-auto w-full max-w-[420px]"
      fontFamily="inherit"
    >
      {children}
    </svg>
  );
}

function useD(id: DiagramId) {
  const { t } = useTranslation();
  return (key: string) => t(`helpInfo.diagram.${id}.${key}`);
}

/** HA·장애 노드 차단: 노드가 죽음 → 차단 → 다른 노드에서 다시 켬 */
function HaFencing() {
  const d = useD("haFencing");
  return (
    <Frame id="haFencing">
      <Step x={14} y={14} n={1} />
      <L x={28} y={14} anchor="start" size={10}>
        {d("step1")}
      </L>
      <Box x={10} y={30} w={90} h={70} tone="bad" label={d("nodeA")} labelY={44} />
      <Vm x={38} y={58} tone="muted" />
      <L x={55} y={88} tone="bad" size={10}>
        {d("dead")}
      </L>
      <Step x={130} y={14} n={2} />
      <L x={144} y={14} anchor="start" size={10}>
        {d("step2")}
      </L>
      <Box x={118} y={48} w={84} h={34} tone="warn" label={d("fence")} />
      <Arrow x1={102} y1={65} x2={116} y2={65} tone="warn" />
      <Step x={222} y={110} n={3} />
      <Box x={220} y={30} w={90} h={70} tone="ok" label={d("nodeB")} labelY={44} />
      <Vm x={248} y={58} tone="ok" />
      <Arrow x1={204} y1={65} x2={218} y2={65} tone="ok" />
      <L x={236} y={110} anchor="start" size={10}>
        {d("step3")}
      </L>
      <L x={160} y={140} tone="muted" size={10}>
        {d("note")}
      </L>
      <L x={160} y={156} tone="muted" size={10}>
        {d("note2")}
      </L>
    </Frame>
  );
}

/** 백업 전체 1번 + 증분(바뀐 부분) 사슬 */
function BackupChain() {
  const d = useD("backupChain");
  const inc = [0, 1, 2, 3];
  return (
    <Frame id="backupChain" h={150}>
      <Box x={10} y={30} w={70} h={56} tone="brand" label={d("full")} labelY={50} />
      <L x={45} y={70} size={9.5} tone="muted">
        {d("fullSize")}
      </L>
      {inc.map((i) => (
        <g key={i}>
          <Arrow x1={82 + i * 58} y1={58} x2={96 + i * 58} y2={58} />
          <Box x={98 + i * 58} y={42} w={40} h={32} tone="ok" label={d("inc")} />
        </g>
      ))}
      <L x={160} y={104} size={10}>
        {d("restore")}
      </L>
      <L x={160} y={124} tone="muted" size={10}>
        {d("note")}
      </L>
    </Frame>
  );
}

/** 원격 복제·DR: 사이트 둘 */
function ReplicationDr() {
  const d = useD("replicationDr");
  return (
    <Frame id="replicationDr" h={160}>
      <Box x={10} y={20} w={96} h={90} tone="brand" label={d("siteA")} labelY={34} />
      <Vm x={18} y={50} />
      <Vm x={62} y={50} />
      <L x={58} y={92} size={10} tone="muted">
        {d("running")}
      </L>
      <Box x={214} y={20} w={96} h={90} dashed label={d("siteB")} labelY={34} />
      <Vm x={222} y={50} tone="muted" label={d("copy")} />
      <Vm x={266} y={50} tone="muted" label={d("copy")} />
      <L x={262} y={92} size={10} tone="muted">
        {d("standby")}
      </L>
      <Arrow x1={108} y1={52} x2={212} y2={52} tone="brand" label={d("replicate")} />
      <Arrow
        x1={212}
        y1={84}
        x2={108}
        y2={84}
        tone="ok"
        dashed
        label={d("failback")}
        labelDy={10}
      />
      <L x={160} y={134} size={10}>
        {d("note")}
      </L>
      <L x={160} y={150} size={10} tone="muted">
        {d("note2")}
      </L>
    </Frame>
  );
}

/** SDN: 노드들 위의 가상 스위치, 라우터, SNAT, 유동 IP */
function Sdn() {
  const d = useD("sdn");
  return (
    <Frame id="sdn" h={180}>
      {[0, 1, 2].map((i) => (
        <g key={i}>
          <Box
            x={10 + i * 70}
            y={120}
            w={62}
            h={50}
            tone="plain"
            label={`${d("node")} ${i + 1}`}
            labelY={160}
          />
          <Vm x={24 + i * 70} y={126} />
          <line
            x1={41 + i * 70}
            y1={126}
            x2={41 + i * 70}
            y2={96}
            stroke="var(--brand)"
            strokeWidth={1.25}
          />
        </g>
      ))}
      <Box x={10} y={80} w={202} h={16} tone="brand" label={d("switch")} />
      <Box x={232} y={76} w={78} h={24} tone="warn" label={d("router")} />
      <Arrow x1={212} y1={88} x2={230} y2={88} tone="warn" />
      <Arrow x1={271} y1={74} x2={271} y2={44} tone="warn" />
      <L x={265} y={59} anchor="end" size={10} tone="warn">
        {d("snat")}
      </L>
      <Box x={232} y={10} w={78} h={32} tone="muted" label={d("internet")} />
      <L x={10} y={22} size={10} anchor="start">
        {d("note")}
      </L>
      <L x={10} y={42} size={10} anchor="start" tone="ok">
        {d("floating")}
      </L>
    </Frame>
  );
}

/** 보안 그룹: 들어오는 규칙(허용·막음), 나가는 것 */
function SecurityGroup() {
  const d = useD("securityGroup");
  return (
    <Frame id="securityGroup" h={160}>
      <Box x={130} y={40} w={70} h={70} tone="brand" label={d("vm")} labelY={60} />
      <rect
        x={122}
        y={32}
        width={86}
        height={86}
        rx={10}
        fill="none"
        stroke="var(--warning)"
        strokeWidth={2}
        strokeDasharray="6 3"
      />
      <L x={165} y={24} size={10} tone="warn">
        {d("group")}
      </L>
      <Arrow x1={10} y1={58} x2={120} y2={58} tone="ok" label={d("allowed")} />
      <Arrow x1={10} y1={96} x2={70} y2={96} tone="bad" label={d("blocked")} />
      <L x={84} y={96} size={14} tone="bad" bold>
        ✕
      </L>
      <Arrow x1={210} y1={75} x2={310} y2={75} tone="muted" label={d("outbound")} />
      <L x={160} y={136} size={10}>
        {d("note")}
      </L>
      <L x={160} y={152} size={10} tone="muted">
        {d("note2")}
      </L>
    </Frame>
  );
}

/** 배치 규칙: 함께(같은 노드) / 떨어져(다른 노드) */
function Placement() {
  const d = useD("placement");
  return (
    <Frame id="placement" h={150}>
      <L x={80} y={14} size={10.5} bold>
        {d("together")}
      </L>
      <Box x={20} y={28} w={120} h={60} label={d("node")} labelY={76} />
      <Vm x={36} y={40} label="app" />
      <Vm x={86} y={40} label="db" />
      <L x={80} y={104} size={10} tone="muted">
        {d("togetherNote")}
      </L>
      <L x={240} y={14} size={10.5} bold>
        {d("apart")}
      </L>
      <Box x={180} y={28} w={58} h={60} label={`${d("node")} 1`} labelY={76} />
      <Box x={244} y={28} w={58} h={60} label={`${d("node")} 2`} labelY={76} />
      <Vm x={192} y={40} label="web1" />
      <Vm x={256} y={40} label="web2" />
      <L x={240} y={104} size={10} tone="muted">
        {d("apartNote")}
      </L>
      <L x={160} y={134} size={10}>
        {d("note")}
      </L>
    </Frame>
  );
}

/** 공유 스토리지와 로컬 스토리지 — 마이그레이션 영향 */
function StorageSharing() {
  const d = useD("storageSharing");
  return (
    <Frame id="storageSharing" h={170}>
      <L x={80} y={12} size={10.5} bold>
        {d("local")}
      </L>
      <Box x={10} y={24} w={60} h={40} label={`${d("node")} A`} />
      <Box x={90} y={24} w={60} h={40} label={`${d("node")} B`} />
      <Box x={14} y={80} w={52} h={24} tone="muted" label={d("disk")} />
      <Box x={94} y={80} w={52} h={24} tone="muted" label={d("disk")} />
      <line x1={40} y1={64} x2={40} y2={80} stroke="var(--muted-foreground)" />
      <line x1={120} y1={64} x2={120} y2={80} stroke="var(--muted-foreground)" />
      <Arrow x1={68} y1={92} x2={92} y2={92} tone="warn" />
      <L x={80} y={122} size={10} tone="warn">
        {d("localMove")}
      </L>
      <L x={240} y={12} size={10.5} bold>
        {d("shared")}
      </L>
      <Box x={170} y={24} w={60} h={40} label={`${d("node")} A`} />
      <Box x={250} y={24} w={60} h={40} label={`${d("node")} B`} />
      <Box x={190} y={80} w={100} h={24} tone="brand" label={d("sharedDisk")} />
      <line x1={200} y1={64} x2={220} y2={80} stroke="var(--brand)" />
      <line x1={280} y1={64} x2={260} y2={80} stroke="var(--brand)" />
      <Arrow x1={232} y1={44} x2={248} y2={44} tone="ok" />
      <L x={240} y={122} size={10} tone="ok">
        {d("sharedMove")}
      </L>
      <L x={160} y={152} size={10} tone="muted">
        {d("note")}
      </L>
    </Frame>
  );
}

/** 씬 프로비전·오버커밋: 약속한 크기 > 실제 디스크 */
function ThinOvercommit() {
  const d = useD("thinOvercommit");
  const vms = [
    { promised: 60, used: 22 },
    { promised: 60, used: 30 },
    { promised: 60, used: 12 },
  ];
  // 1 GiB = 1.5px, 실제 디스크 100 GiB = 150px
  return (
    <Frame id="thinOvercommit" h={160}>
      <L x={10} y={14} anchor="start" size={10.5} bold>
        {d("promised")}
      </L>
      {vms.map((vm, i) => (
        <g key={i}>
          <rect
            x={10 + i * 92}
            y={24}
            width={vm.promised * 1.5}
            height={18}
            rx={3}
            fill="none"
            stroke="var(--muted-foreground)"
            strokeDasharray="4 3"
          />
          <rect
            x={10 + i * 92}
            y={24}
            width={vm.used * 1.5}
            height={18}
            rx={3}
            fill="var(--brand)"
            opacity={0.7}
          />
          <L x={10 + i * 92 + 45} y={52} size={9.5} tone="muted">
            {`VM ${i + 1}: 60 GiB`}
          </L>
        </g>
      ))}
      <L x={10} y={78} anchor="start" size={10.5} bold>
        {d("actual")}
      </L>
      <rect
        x={10}
        y={88}
        width={150}
        height={18}
        rx={3}
        fill="var(--muted)"
        stroke="var(--muted-foreground)"
      />
      <rect x={10} y={88} width={64 * 1.5} height={18} rx={3} fill="var(--brand)" opacity={0.7} />
      <L x={170} y={97} anchor="start" size={10}>
        {d("actualNote")}
      </L>
      <L x={160} y={128} size={10}>
        {d("note")}
      </L>
      <L x={160} y={146} size={10} tone="warn">
        {d("warn")}
      </L>
    </Frame>
  );
}

/** 라이브 마이그레이션: 켠 채로 메모리를 옮긴다 */
function LiveMigration() {
  const d = useD("liveMigration");
  return (
    <Frame id="liveMigration" h={160}>
      <Box x={10} y={20} w={90} h={64} label={`${d("node")} A`} labelY={34} />
      <Vm x={38} y={52} />
      <Box x={220} y={20} w={90} h={64} label={`${d("node")} B`} labelY={34} />
      <Vm x={248} y={52} tone="ok" />
      <Arrow x1={102} y1={52} x2={218} y2={52} tone="brand" label={d("copy")} />
      <Arrow
        x1={102}
        y1={72}
        x2={218}
        y2={72}
        tone="brand"
        dashed
        label={d("delta")}
        labelDy={10}
      />
      <Step x={20} y={106} n={1} />
      <L x={32} y={106} anchor="start" size={10}>
        {d("step1")}
      </L>
      <Step x={20} y={126} n={2} />
      <L x={32} y={126} anchor="start" size={10}>
        {d("step2")}
      </L>
      <Step x={20} y={146} n={3} />
      <L x={32} y={146} anchor="start" size={10}>
        {d("step3")}
      </L>
    </Frame>
  );
}

/** 코드로 관리(선언 적용): 파일 → 차이 → 적용 */
function DeclarativeApply() {
  const d = useD("declarativeApply");
  return (
    <Frame id="declarativeApply" h={150}>
      <Box x={10} y={30} w={76} h={60} tone="muted" label={d("file")} labelY={44} />
      <L x={48} y={64} size={9.5} tone="muted">
        vms.yaml
      </L>
      <Arrow x1={88} y1={60} x2={112} y2={60} />
      <Box x={114} y={22} w={92} h={76} tone="brand" label={d("diff")} labelY={36} />
      <L x={124} y={56} anchor="start" size={10} tone="ok">
        {`+ ${d("add")}`}
      </L>
      <L x={124} y={72} anchor="start" size={10} tone="warn">
        {`~ ${d("change")}`}
      </L>
      <L x={124} y={88} anchor="start" size={10} tone="bad">
        {`− ${d("remove")}`}
      </L>
      <Arrow x1={208} y1={60} x2={232} y2={60} tone="ok" label={d("apply")} />
      <Box x={234} y={30} w={76} h={60} tone="ok" label={d("cluster")} />
      <L x={160} y={120} size={10}>
        {d("note")}
      </L>
      <L x={160} y={138} size={10} tone="muted">
        {d("note2")}
      </L>
    </Frame>
  );
}

/** 백업 저장소: 같은 조각은 한 번만 */
function DedupStore() {
  const d = useD("dedupStore");
  // 백업마다 조각 색(같은 색 = 같은 내용)
  const backups = [
    ["brand", "ok", "warn"],
    ["brand", "ok", "bad"],
    ["brand", "ok", "warn"],
  ] as const;
  const unique = ["brand", "ok", "warn", "bad"] as const;
  return (
    <Frame id="dedupStore" h={150}>
      {backups.map((chunks, i) => (
        <g key={i}>
          <L x={10} y={26 + i * 30} anchor="start" size={10}>
            {`${d("backup")} ${i + 1}`}
          </L>
          {chunks.map((tone, j) => (
            <rect
              key={j}
              x={70 + j * 18}
              y={18 + i * 30}
              width={14}
              height={16}
              rx={2}
              fill={STROKE[tone]}
              opacity={0.75}
            />
          ))}
        </g>
      ))}
      <Arrow x1={128} y1={56} x2={204} y2={56} label={d("dedup")} />
      <Box x={208} y={20} w={102} h={72} tone="plain" label={d("store")} labelY={34} />
      {unique.map((tone, j) => (
        <rect
          key={j}
          x={218 + j * 22}
          y={52}
          width={16}
          height={18}
          rx={2}
          fill={STROKE[tone]}
          opacity={0.75}
        />
      ))}
      <L x={160} y={116} size={10}>
        {d("note")}
      </L>
      <L x={160} y={134} size={10} tone="muted">
        {d("note2")}
      </L>
    </Frame>
  );
}

/** 클러스터 투표(과반): 노드 셋 중 둘이 살아야 운영 */
function Quorum() {
  const d = useD("quorum");
  const nodes: { x: number; tone: Tone; label: string }[] = [
    { x: 60, tone: "ok", label: d("vote") },
    { x: 160, tone: "ok", label: d("vote") },
    { x: 260, tone: "bad", label: d("down") },
  ];
  return (
    <Frame id="quorum" h={150}>
      {nodes.map((n, i) => (
        <g key={i}>
          <circle
            cx={n.x}
            cy={50}
            r={26}
            fill={FILL[n.tone]}
            stroke={STROKE[n.tone]}
            strokeWidth={1.5}
          />
          <L x={n.x} y={44} size={10.5}>
            {`${d("node")} ${i + 1}`}
          </L>
          <L x={n.x} y={58} size={9.5} tone={n.tone}>
            {n.label}
          </L>
        </g>
      ))}
      <L x={160} y={98} size={10.5} tone="ok" bold>
        {d("majority")}
      </L>
      <L x={160} y={118} size={10}>
        {d("note")}
      </L>
      <L x={160} y={136} size={10} tone="muted">
        {d("note2")}
      </L>
    </Frame>
  );
}

const DIAGRAMS: Record<DiagramId, () => React.ReactNode> = {
  haFencing: HaFencing,
  backupChain: BackupChain,
  replicationDr: ReplicationDr,
  sdn: Sdn,
  securityGroup: SecurityGroup,
  placement: Placement,
  storageSharing: StorageSharing,
  thinOvercommit: ThinOvercommit,
  liveMigration: LiveMigration,
  declarativeApply: DeclarativeApply,
  dedupStore: DedupStore,
  quorum: Quorum,
};

export default function Diagram({ id }: { id: DiagramId }) {
  const Shown = DIAGRAMS[id];
  return <Shown />;
}
