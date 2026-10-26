/**
 * 테스트용 `XMLHttpRequest`. jsdom은 업로드 진행률을 내지 않는다. `vi.stubGlobal("XMLHttpRequest",
 * FakeXhr)`로 바꿔 끼우고, 마지막 요청(`FakeXhr.last`)에 진행률과 응답을 흉내 낸다.
 */
export class FakeXhr {
  static last: FakeXhr | undefined;
  method = "";
  url = "";
  headers: Record<string, string> = {};
  body: unknown;
  status = 0;
  responseText = "";
  aborted = false;
  upload: { onprogress: ((e: { loaded: number; total: number }) => void) | null } = {
    onprogress: null,
  };
  onload: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onabort: (() => void) | null = null;

  constructor() {
    FakeXhr.last = this;
  }

  open(method: string, url: string) {
    this.method = method;
    this.url = url;
  }

  setRequestHeader(name: string, value: string) {
    this.headers[name] = value;
  }

  send(body: unknown) {
    this.body = body;
  }

  abort() {
    this.aborted = true;
    this.onabort?.();
  }

  progress(loaded: number, total: number) {
    this.upload.onprogress?.({ loaded, total });
  }

  respond(status: number, body: unknown) {
    this.status = status;
    this.responseText = typeof body === "string" ? body : JSON.stringify(body);
    this.onload?.();
  }

  fail() {
    this.onerror?.();
  }
}
