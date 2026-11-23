// QR 그림(SVG 칸). 2단계 인증 등록(`otpauth://`)과 백업 저장소 복구 키(계획 68)가 함께 쓴다.
import qrcode from "qrcode-generator";
import { useMemo } from "react";

/** 글의 QR(SVG 칸). 화면 읽기에는 이름표만 읽고 비밀은 따로 보인다. */
export function QrCode({ text, label }: { text: string; label: string }) {
  const cells = useMemo(() => {
    const qr = qrcode(0, "M");
    qr.addData(text);
    qr.make();
    const n = qr.getModuleCount();
    const dark: string[] = [];
    for (let r = 0; r < n; r++) {
      for (let c = 0; c < n; c++) {
        if (qr.isDark(r, c)) {
          dark.push(`M${c + 4} ${r + 4}h1v1h-1z`);
        }
      }
    }
    return { n, path: dark.join("") };
  }, [text]);
  const size = cells.n + 8;
  return (
    <svg
      role="img"
      aria-label={label}
      viewBox={`0 0 ${size} ${size}`}
      className="size-48 rounded-md bg-white"
      shapeRendering="crispEdges"
    >
      <rect width={size} height={size} fill="#fff" />
      <path d={cells.path} fill="#000" />
    </svg>
  );
}
