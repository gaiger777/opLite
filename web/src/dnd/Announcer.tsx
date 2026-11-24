/**
 * 알림 줄(계획 71 설계 §6.1·§11): 들기·칸 위·놓기·취소와 쓰는 쪽의 한 마디를 읽는 `aria-live="polite"` 하나. 쪽마다
 * 하나 둔다(`DndProvider`가 그린다).
 */
export function Announcer({ text }: { text: string }) {
  return (
    <div aria-live="polite" role="status" className="sr-only">
      {text}
    </div>
  );
}
