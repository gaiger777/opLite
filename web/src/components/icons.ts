// 아이콘 표(설계 §6). 같은 뜻에는 어디서나 같은 lucide 아이콘을 쓴다. 새 화면은 여기서 고른다.
// 크기는 표·메뉴 16px(`ICON_SIZE`), 사이드바 레일 20px(`RAIL_ICON_SIZE`)이고, 선 굵기 1.75는
// `index.css`의 `svg.lucide`가 정한다.
import {
  ArrowLeftRight,
  Boxes,
  Bell,
  Camera,
  Copy,
  Database,
  Disc3,
  Download,
  Ellipsis,
  Eye,
  History,
  Keyboard,
  Languages,
  LayoutDashboard,
  LogOut,
  Network,
  Palette,
  Pencil,
  Play,
  Plus,
  Power,
  RefreshCw,
  RotateCcw,
  RotateCw,
  Rows3,
  Search,
  Server,
  Settings,
  ShieldCheck,
  Square,
  SquareTerminal,
  Star,
  Trash2,
  Upload,
  UserRound,
  type LucideIcon,
} from "lucide-react";

/** 사이드바의 묶음(계획 77 — 진단 §1.3). `account`는 사이드바에 없고 사용자 메뉴에서 연다. */
export type GroupId =
  | "overview"
  | "vms"
  | "images"
  | "storage"
  | "network"
  | "cluster"
  | "backup"
  | "records"
  | "admin"
  | "account";

export const GROUP_ICONS: Record<GroupId, LucideIcon> = {
  overview: LayoutDashboard,
  vms: Server,
  images: Disc3,
  storage: Database,
  network: Network,
  cluster: Boxes,
  backup: ShieldCheck,
  records: History,
  admin: Settings,
  account: UserRound,
};

/** 동작의 아이콘. 메뉴·단추·팔레트·일괄 막대가 함께 쓴다. */
export const ACTION_ICONS = {
  create: Plus,
  edit: Pencil,
  delete: Trash2,
  copy: Copy,
  upload: Upload,
  download: Download,
  refresh: RefreshCw,
  more: Ellipsis,
  search: Search,
  peek: Eye,
  favorite: Star,
  notifications: Bell,
  start: Play,
  stop: Square,
  forceStop: Power,
  reboot: RotateCw,
  migrate: ArrowLeftRight,
  snapshot: Camera,
  console: SquareTerminal,
  retry: RotateCcw,
  density: Rows3,
  theme: Palette,
  language: Languages,
  shortcuts: Keyboard,
  logout: LogOut,
} satisfies Record<string, LucideIcon>;

export type ActionId = keyof typeof ACTION_ICONS;

/** 표·메뉴·단추 안 아이콘 */
export const ICON_SIZE = "size-4";
/** 사이드바 레일 아이콘 */
export const RAIL_ICON_SIZE = "size-5";
