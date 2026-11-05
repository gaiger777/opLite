import { XIcon } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";

import { useSubscription } from "@/api/live";
import type { SubscriptionInfo } from "@/api/types";
import { Button } from "@/components/ui/button";
import { formatDate } from "@/lib/format";
import { readPref, writePref } from "@/lib/prefs";

const DAY_MS = 24 * 60 * 60 * 1000;
/** 만료가 이만큼 남으면 "곧 만료"를 보인다. */
const EXPIRING_MS = 30 * DAY_MS;
/** 닫으면 이 브라우저에서 이만큼 숨긴다. */
const HIDE_MS = 7 * DAY_MS;
export const HIDDEN_UNTIL_PREF = "subscription-banner-hidden-until";

type Notice = "none" | "invalid" | "expired" | "over_limit" | "expiring";

/** 보일 안내. active이고 만료가 30일보다 많이 남았으면 없다. */
export function subscriptionNotice(info: SubscriptionInfo, now: number): Notice | null {
  if (info.state !== "active") {
    return info.state;
  }
  return info.expires_at !== undefined && info.expires_at * 1000 - now <= EXPIRING_MS
    ? "expiring"
    : null;
}

/** 닫은 안내와 그 안내를 숨길 시각(`"<안내>:<ms>"`). 옛 판의 숫자만 있는 값은 없는 것으로 본다. */
function dismissed(): { notice: string; until: number } | null {
  const [notice, until] = (readPref(HIDDEN_UNTIL_PREF) ?? "").split(":");
  return notice && until ? { notice, until: Number(until) || 0 } : null;
}

/**
 * 대시보드 위의 구독 안내. 닫으면 그 안내만 7일 동안 숨긴다(다른 안내가 되면 보인다). 상태를 읽지 못하면
 * 아무것도 보이지 않는다.
 */
export function SubscriptionBanner() {
  const { t, i18n } = useTranslation();
  const subscription = useSubscription();
  // 지금 시각은 구독 상태를 다시 읽을 때(한 시간마다) 함께 바뀐다.
  const now = subscription.dataUpdatedAt;
  const [hidden, setHidden] = useState(dismissed);

  const notice = subscription.data && subscriptionNotice(subscription.data, now);
  if (!notice || (hidden?.notice === notice && hidden.until > now)) {
    return null;
  }
  const info = subscription.data!;
  const warning = notice === "expired" || notice === "over_limit";
  const date =
    info.expires_at === undefined
      ? ""
      : formatDate(new Date(info.expires_at * 1000), i18n.language);

  return (
    <div
      role={warning ? "alert" : "status"}
      className={
        warning
          ? "flex items-center gap-3 rounded-md border border-amber-500/50 bg-amber-500/10 px-3 py-2 text-sm"
          : "flex items-center gap-3 rounded-md border border-border bg-muted/50 px-3 py-2 text-sm"
      }
    >
      <p className="flex-1">
        {t(`dashboard.subscription.${notice}`, {
          date,
          used: info.used_nodes,
          limit: info.nodes,
        })}{" "}
        <Link to="/settings" className="underline underline-offset-4">
          {t("dashboard.subscription.manage")}
        </Link>
      </p>
      <Button
        variant="ghost"
        size="icon-sm"
        aria-label={t("dashboard.subscription.dismiss")}
        onClick={() => {
          const until = Date.now() + HIDE_MS;
          writePref(HIDDEN_UNTIL_PREF, `${notice}:${until}`);
          setHidden({ notice, until });
        }}
      >
        <XIcon />
      </Button>
    </div>
  );
}
