// noVNC는 타입 정의를 싣지 않는다. 이 앱이 쓰는 부분만 적는다.
declare module "@novnc/novnc" {
  export default class RFB extends EventTarget {
    constructor(target: HTMLElement, url: string, options?: { wsProtocols?: string[] });
    scaleViewport: boolean;
    resizeSession: boolean;
    disconnect(): void;
    sendCtrlAltDel(): void;
    sendKey(keysym: number, code: string | null, down?: boolean): void;
    focus(): void;
  }
}
