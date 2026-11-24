/**
 * 브라우저가 스스로 내려받게 한다(큰 zip을 JS 메모리에 담지 않는다, 설계 §11). `download` 속성이 있는 링크라 화면을 떠나지
 * 않는다(세션·진행 중인 다른 내려받기가 그대로다). 링크는 한 번만 쓰는 60초 토큰이고 쿠키로 같은 사용자를 다시 본다.
 * 시험은 이 모듈을 바꿔 끼운다(jsdom은 이동을 하지 못한다).
 */
export function startDownload(href: string) {
  const a = document.createElement("a");
  a.href = href;
  a.download = "";
  a.rel = "noopener";
  a.style.display = "none";
  document.body.appendChild(a);
  a.click();
  a.remove();
}
