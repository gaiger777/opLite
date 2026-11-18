// 아이콘 표(설계 §6). 같은 뜻에는 어디서나 같은 lucide 아이콘을 쓴다. 새 화면은 여기서 고른다.
// 크기는 표·메뉴 16px(`ICON_SIZE`), 사이드바 레일 20px(`RAIL_ICON_SIZE`)이고, 선 굵기 1.75는
// `index.css`의 `svg.lucide`가 정한다.
import {
  Activity,
  ArrowLeftRight,
  Bell,
  Camera,
  Copy,
  Download,
  Ellipsis,
  Eye,
  House,
  Keyboard,
  Languages,
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
  type LucideIcon,
} from "lucide-react";

/** 사이드바의 최상위 영역(설계 §4.2) */
export type AreaId = "home" | "vms" | "infra" | "protect" | "activity" | "settings";

export const AREA_ICONS: Record<AreaId, LucideIcon> = {
  home: House,
  vms: Server,
  infra: Network,
  protect: ShieldCheck,
  activity: Activity,
  settings: Settings,
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
