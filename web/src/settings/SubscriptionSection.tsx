import { useQueryClient } from "@tanstack/react-query";
import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import { SETTINGS_KEY, SUBSCRIPTION_KEY, useSubscription } from "@/api/live";
import type { SubscriptionInfo, SubscriptionState } from "@/api/types";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Field } from "@/components/Field";
import { ListPending } from "@/components/ListPending";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { formatDate } from "@/lib/format";
import { settingsError, withSettingsError } from "@/settings/save";

const BADGE: Record<SubscriptionState, "default" | "destructive" | "outline"> = {
  active: "default",
  expired: "destructive",
  over_limit: "destructive",
  none: "outline",
  invalid: "outline",
};

/** 구독 상태 카드. admin은 키를 넣고 지운다. 키 문자열은 서버가 돌려주지 않는다. */
export function SubscriptionSection({ write }: { write: boolean }) {
  const { t, i18n } = useTranslation();
  const queryClient = useQueryClient();
  const subscription = useSubscription();
  const [key, setKey] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [removing, setRemoving] = useState(false);
  // 대화상자를 닫으면 포커스를 돌려줄 곳: 지우기 버튼, 지워서 버튼이 사라지면 이 부분의 제목
  const returnFocus = useRef<HTMLElement | null>(null);
  const heading = useRef<HTMLHeadingElement>(null);

  const info = subscription.data;
  const store = (updated: SubscriptionInfo) => {
    queryClient.setQueryData(SUBSCRIPTION_KEY, updated);
    // 구독 키는 설정에 있다(watch 이벤트를 기다리지 않고 다시 읽는다).
    void queryClient.invalidateQueries({ queryKey: SETTINGS_KEY });
  };

  async function apply(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      store(await call(api.PUT("/subscription", { body: { key: key.trim() } })));
      setKey("");
      toast.success(t("settings.subscription.applied"));
    } catch (err) {
      setError(settingsError(t, err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section aria-labelledby="settings-subscription" className="flex flex-col gap-4">
      <h2 id="settings-subscription" ref={heading} tabIndex={-1} className="font-medium">
        {t("settings.subscription.title")}
      </h2>
      {!info ? (
        <ListPending error={subscription.error} />
      ) : (
        <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm">
          <dt className="text-muted-foreground">{t("common.state")}</dt>
          <dd className="flex items-center gap-1">
            <Badge variant={BADGE[info.state]}>
              {t(`settings.subscription.states.${info.state}`)}
            </Badge>
            {info.development && (
              <Badge variant="outline">{t("settings.subscription.development")}</Badge>
            )}
          </dd>
          {info.customer && (
            <>
              <dt className="text-muted-foreground">{t("settings.subscription.customer")}</dt>
              <dd>{info.customer}</dd>
            </>
          )}
          {info.level && (
            <>
              <dt className="text-muted-foreground">{t("settings.subscription.level")}</dt>
              <dd>{t(`settings.subscription.levels.${info.level}`)}</dd>
            </>
          )}
          <dt className="text-muted-foreground">{t("settings.subscription.nodes")}</dt>
          <dd>
            {info.nodes === undefined ? info.used_nodes : `${info.used_nodes} / ${info.nodes}`}
          </dd>
          {info.expires_at !== undefined && (
            <>
              <dt className="text-muted-foreground">{t("settings.subscription.expires")}</dt>
              <dd>{formatDate(new Date(info.expires_at * 1000), i18n.language)}</dd>
            </>
          )}
        </dl>
      )}
      {write && (
        <form noValidate onSubmit={(e) => void apply(e)} className="flex flex-col gap-4">
          {error && (
            <p role="alert" className="text-sm text-destructive">
              {error}
            </p>
          )}
          <Field
            id="settings-subscription-key"
            label={t("settings.subscription.key")}
            hint={t("settings.subscription.keyHint")}
          >
            {(a11y) => (
              <textarea
                id="settings-subscription-key"
                rows={3}
                spellCheck={false}
                autoComplete="off"
                value={key}
                disabled={busy}
                onChange={(e) => setKey(e.target.value)}
                className="w-full min-w-0 rounded-lg border border-input bg-transparent px-2.5 py-1 font-mono text-xs break-all outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50 disabled:opacity-50 dark:bg-input/30"
                {...a11y}
              />
            )}
          </Field>
          <div className="flex gap-2">
            <Button type="submit" disabled={busy || key.trim() === ""}>
              {t("settings.subscription.apply")}
            </Button>
            {info && info.state !== "none" && (
              <Button
                type="button"
                variant="outline"
                disabled={busy}
                onClick={(e) => {
                  returnFocus.current = e.currentTarget;
                  setRemoving(true);
                }}
              >
                {t("settings.subscription.remove")}
              </Button>
            )}
          </div>
        </form>
      )}
      <ConfirmDialog
        open={removing}
        onOpenChange={setRemoving}
        title={t("settings.subscription.removeTitle")}
        description={t("settings.subscription.removeDescription")}
        confirmLabel={t("settings.subscription.remove")}
        returnFocus={returnFocus}
        onConfirm={() =>
          withSettingsError(t, async () => {
            store(await call(api.DELETE("/subscription")));
            returnFocus.current = heading.current;
            toast.success(t("settings.subscription.removed"));
          })
        }
      />
    </section>
  );
}
