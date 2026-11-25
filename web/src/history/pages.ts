// 설정 기록·선언 묶음·인증서 보기(계획 73)의 늦게 읽는 조각 하나: 서로 계획 표·이력 서랍을 같이 쓰므로 한 조각으로 받는다
// (경로마다 나누면 첫 화면 묶음의 경로 표와 공유 조각이 커진다 — 계획 72 `vmware/pages.ts`와 같은 판단).
export { ApplySetPage } from "@/declarative/ApplySetPage";
export { ApplySetsPage } from "@/declarative/ApplySetsPage";
export { CertificatesPage } from "@/certs/CertificatesPage";
export { HistoryPage } from "@/history/HistoryPage";
export { loadCertStrings, loadDeclarativeStrings } from "@/declarative/strings";
