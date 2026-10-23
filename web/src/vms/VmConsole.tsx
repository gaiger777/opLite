import RFB from "@novnc/novnc";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import type { Vm } from "@/api/types";
import { Button } from "@/components/ui/button";
import { keyEvents } from "@/vms/keysyms";

type State = "connecting" | "connected" | "failed";

/** 한 번에 입력할 최대 글자 수 */
export const PASTE_LIMIT = 4096;

export function consoleUrl(vm: Vm) {
  const scheme = window.location.protocol === "https:" ? "wss" : "ws";
  return `${scheme}://${window.location.host}/api/v1/vms/${encodeURIComponent(vm.id)}/console`;
}

/**
 * VM 화면(VNC). API가 WebSocket으로 RFB를 그대로 넘기므로 noVNC가 곧바로 붙는다. 세션 쿠키는 같은
 * 출처라 브라우저가 붙인다. 연결이 끊기면 다시 연결 버튼을 보여 준다.
 */
export default function VmConsole({ vm }: { vm: Vm }) {
  const { t } = useTranslation();
  const screen = useRef<HTMLDivElement>(null);
  const rfb = useRef<RFB | null>(null);
  const [state, setState] = useState<State>("connecting");
  const [attempt, setAttempt] = useState(0);
  const running = vm.status.phase === "running";

  useEffect(() => {
    if (!running || !screen.current) {
      return;
    }
    setState("connecting");
    const client = new RFB(screen.current, consoleUrl(vm));
    client.scaleViewport = true;
    // disconnect()의 이벤트는 나중에 온다. 다시 연결한 뒤 앞 연결의 이벤트는 무시한다.
    const set = (next: State) => () => {
      if (rfb.current === client) {
        setState(next);
      }
    };
    client.addEventListener("connect", set("connected"));
    client.addEventListener("disconnect", set("failed"));
    rfb.current = client;
    return () => {
      rfb.current = null;
      client.disconnect();
    };
    // VM id가 같으면 상태가 바뀌어도 다시 연결하지 않는다.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [vm.id, running, attempt]);

  async function paste() {
    try {
      const chars = Array.from(await navigator.clipboard.readText());
      for (const [sym, code, down] of keyEvents(chars.slice(0, PASTE_LIMIT).join(""))) {
        rfb.current?.sendKey(sym, code, down);
      }
      if (chars.length > PASTE_LIMIT) {
        toast.warning(t("console.pasteTruncated", { count: PASTE_LIMIT }));
      }
      rfb.current?.focus();
    } catch {
      toast.error(t("console.pasteDenied"));
    }
  }

  if (!running) {
    return (
      <p role="status" className="text-sm text-muted-foreground">
        {t("errors.vm_not_running")}
      </p>
    );
  }
  return (
    <div className="flex flex-col gap-2">
      <div className="flex flex-wrap items-center gap-2 text-sm">
        <span role="status" className="mr-2 text-muted-foreground">
          {t(`console.${state}`)}
        </span>
        {state === "failed" && (
          <Button size="sm" variant="outline" onClick={() => setAttempt((n) => n + 1)}>
            {t("console.reconnect")}
          </Button>
        )}
        <Button
          size="sm"
          variant="outline"
          disabled={state !== "connected"}
          onClick={() => rfb.current?.sendCtrlAltDel()}
        >
          Ctrl+Alt+Del
        </Button>
        <Button
          size="sm"
          variant="outline"
          disabled={state !== "connected"}
          onClick={() => void paste()}
        >
          {t("console.paste")}
        </Button>
        {document.fullscreenEnabled && (
          <Button
            size="sm"
            variant="outline"
            onClick={() => void screen.current?.requestFullscreen().catch(() => {})}
          >
            {t("console.fullscreen")}
          </Button>
        )}
        <span className="text-xs text-muted-foreground">{t("console.hint")}</span>
      </div>
      <div
        ref={screen}
        role="region"
        aria-label={t("console.screen")}
        data-testid="console-screen"
        className="aspect-[4/3] max-h-[80vh] w-full overflow-hidden rounded-md bg-black"
      />
    </div>
  );
}
