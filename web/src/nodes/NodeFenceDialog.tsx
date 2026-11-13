import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ArrowDown, ArrowUp, Plus, X } from "lucide-react";
import { type RefObject, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { ApiError, api, call } from "@/api/client";
import type { components } from "@/api/schema";
import type { Node } from "@/api/types";
import { Field } from "@/components/Field";
import { focusBack } from "@/components/focusBack";
import { ListPending } from "@/components/ListPending";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { codeMessage, errorMessage } from "@/i18n/errors";
import {
  emptyRow,
  type FenceKind,
  type FenceRow,
  fenceErrors,
  fromView,
  MAX_FENCE_METHODS,
  toMethod,
} from "@/nodes/fence";
import { moveRule } from "@/security-groups/rules";

type FenceView = components["schemas"]["NodeFenceView"];
type FenceTest = components["schemas"]["FenceTestResult"];

const KINDS: FenceKind[] = ["ipmi", "redfish", "command"];

/** 펜싱 대화상자의 오류 문구. `nodes.fence.errors.<code>`를 먼저 찾는다. */
function fenceError(t: Parameters<typeof errorMessage>[0], err: unknown): string {
  return err instanceof ApiError
    ? codeMessage(t, err.code, err.message, "nodes.fence.errors")
    : errorMessage(t, err);
}

/**
 * 노드의 전원 끄기 펜싱(admin). 방법을 순서대로 고치고 저장하며, 저장된 설정으로 전원을 읽어 본다(끄지 않는다).
 * 연 쪽은 열 때만 그린다.
 */
export function NodeFenceDialog({
  node,
  onClose,
  returnFocus,
}: {
  node: Node;
  onClose(): void;
  returnFocus?: RefObject<HTMLElement | null>;
}) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const key = ["node-fence", node.id];
  // 설정이 없으면 404이고 빈 목록으로 시작한다.
  const saved = useQuery({
    queryKey: key,
    queryFn: async (): Promise<FenceView | null> => {
      try {
        return await call(api.GET("/nodes/{key}/fence", { params: { path: { key: node.id } } }));
      } catch (err) {
        if (err instanceof ApiError && err.status === 404) {
          return null;
        }
        throw err;
      }
    },
  });
  const [edited, setEdited] = useState<FenceRow[] | null>(null);
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [test, setTest] = useState<FenceTest | null>(null);

  const view = saved.data ?? null;
  const rows = edited ?? (view ? view.methods.map(fromView) : []);
  const change = (next: FenceRow[]) => {
    setEdited(next);
    setTest(null);
  };
  const setRow = (i: number, patch: Partial<FenceRow>) =>
    change(rows.map((r, j) => (j === i ? { ...r, ...patch } : r)));

  async function run(action: () => Promise<void>) {
    setBusy(true);
    setError(null);
    try {
      await action();
    } catch (err) {
      setError(fenceError(t, err));
    } finally {
      setBusy(false);
    }
  }

  async function save(e: React.FormEvent) {
    e.preventDefault();
    const found = fenceErrors(rows);
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return;
    }
    await run(async () => {
      const updated = await call(
        api.PUT("/nodes/{key}/fence", {
          params: {
            path: { key: node.id },
            header: view ? { "If-Match": view.revision } : {},
          },
          body: { methods: rows.map(toMethod) },
        }),
      );
      queryClient.setQueryData(key, updated);
      setEdited(null);
      toast.success(t("nodes.fence.saved", { name: node.name }));
    });
  }

  async function remove() {
    await run(async () => {
      await call(
        api.DELETE("/nodes/{key}/fence", {
          params: { path: { key: node.id }, header: { "If-Match": view!.revision } },
        }),
      );
      queryClient.setQueryData(key, null);
      setEdited(null);
      toast.success(t("nodes.fence.removed", { name: node.name }));
    });
  }

  async function check() {
    await run(async () => {
      setTest(
        await call(api.POST("/nodes/{key}/fence-test", { params: { path: { key: node.id } } })),
      );
    });
  }

  return (
    <Dialog open onOpenChange={(open) => !open && !busy && onClose()}>
      <DialogContent className="sm:max-w-2xl" onCloseAutoFocus={(e) => focusBack(e, returnFocus)}>
        <form noValidate onSubmit={(e) => void save(e)} className="flex flex-col gap-4">
          <DialogHeader>
            <DialogTitle>{t("nodes.fence.title", { name: node.name })}</DialogTitle>
            <DialogDescription>{t("nodes.fence.description")}</DialogDescription>
          </DialogHeader>
          {error && (
            <p role="alert" className="text-sm text-destructive">
              {error}
            </p>
          )}
          {saved.isPending || saved.isError ? (
            <ListPending error={saved.error} />
          ) : (
            <fieldset disabled={busy} className="flex min-w-0 flex-col gap-4">
              {rows.length === 0 && (
                <p className="text-sm text-muted-foreground">{t("nodes.fence.none")}</p>
              )}
              <ol className="flex flex-col gap-4">
                {rows.map((row, i) => (
                  <MethodRow
                    key={i}
                    row={row}
                    i={i}
                    last={i === rows.length - 1}
                    errors={errors}
                    onChange={(patch) => setRow(i, patch)}
                    onKind={(kind) => change(rows.map((r, j) => (j === i ? emptyRow(kind) : r)))}
                    onMove={(by) => change(moveRule(rows, i, by))}
                    onRemove={() => change(rows.filter((_, j) => j !== i))}
                  />
                ))}
              </ol>
              {rows.length < MAX_FENCE_METHODS && (
                <div>
                  <Button
                    type="button"
                    variant="outline"
                    size="sm"
                    onClick={() => change([...rows, emptyRow("ipmi")])}
                  >
                    <Plus />
                    {t("nodes.fence.add")}
                  </Button>
                </div>
              )}
              <p className="text-xs text-muted-foreground">{t("nodes.fence.note")}</p>
            </fieldset>
          )}
          {test && <TestResult test={test} />}
          <DialogFooter>
            {view && (
              <>
                <Button
                  type="button"
                  variant="destructive"
                  disabled={busy}
                  onClick={() => void remove()}
                >
                  {t("nodes.fence.remove")}
                </Button>
                {/* 저장된 설정으로 읽는다. */}
                <Button
                  type="button"
                  variant="outline"
                  disabled={busy || edited !== null}
                  onClick={() => void check()}
                >
                  {t("nodes.fence.test")}
                </Button>
              </>
            )}
            <Button type="button" variant="outline" disabled={busy} onClick={onClose}>
              {t("common.cancel")}
            </Button>
            <Button type="submit" disabled={busy || edited === null || rows.length === 0}>
              {t("common.save")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

/** 방법 한 줄: 종류와 그 종류의 칸, 순서 바꾸기·빼기 */
function MethodRow({
  row,
  i,
  last,
  errors,
  onChange,
  onKind,
  onMove,
  onRemove,
}: {
  row: FenceRow;
  i: number;
  last: boolean;
  errors: Record<string, string>;
  onChange(patch: Partial<FenceRow>): void;
  onKind(kind: FenceKind): void;
  onMove(by: -1 | 1): void;
  onRemove(): void;
}) {
  const { t } = useTranslation();
  const n = { n: i + 1 };
  const id = (field: string) => `fence-${i}-${field}`;
  const text = (field: keyof FenceRow & string, label: string, hint?: string) => (
    <Field id={id(field)} label={label} error={errors[`${i}.${field}`]} hint={hint}>
      {(a11y) => (
        <Input
          id={id(field)}
          autoComplete="off"
          value={row[field] as string}
          onChange={(e) => onChange({ [field]: e.target.value })}
          {...a11y}
        />
      )}
    </Field>
  );
  const password = (
    <>
      <Field
        id={id("password")}
        label={t("nodes.fence.password")}
        hint={row.hasPassword ? t("nodes.fence.passwordSaved") : undefined}
      >
        {(a11y) => (
          <Input
            id={id("password")}
            type="password"
            autoComplete="new-password"
            value={row.password}
            disabled={row.clearPassword}
            onChange={(e) => onChange({ password: e.target.value })}
            {...a11y}
          />
        )}
      </Field>
      {row.hasPassword && (
        <div className="flex items-center gap-2 sm:col-span-2">
          <Checkbox
            id={id("clearPassword")}
            checked={row.clearPassword}
            onCheckedChange={(v) => onChange({ clearPassword: v === true, password: "" })}
          />
          <Label htmlFor={id("clearPassword")}>{t("nodes.fence.clearPassword")}</Label>
        </div>
      )}
    </>
  );
  return (
    <li
      aria-label={t("nodes.fence.method", n)}
      className="flex flex-col gap-3 rounded-lg border p-3"
    >
      <div className="flex items-end gap-2">
        <Field id={id("kind")} label={t("nodes.fence.kind", n)}>
          <NativeSelect
            id={id("kind")}
            value={row.kind}
            onChange={(e) => onKind(e.target.value as FenceKind)}
          >
            {KINDS.map((k) => (
              <NativeSelectOption key={k} value={k}>
                {t(`nodes.fence.kinds.${k}`)}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        </Field>
        <div className="ml-auto flex">
          <Button
            type="button"
            variant="ghost"
            size="icon-sm"
            aria-label={t("nodes.fence.moveUp", n)}
            disabled={i === 0}
            onClick={() => onMove(-1)}
          >
            <ArrowUp />
          </Button>
          <Button
            type="button"
            variant="ghost"
            size="icon-sm"
            aria-label={t("nodes.fence.moveDown", n)}
            disabled={last}
            onClick={() => onMove(1)}
          >
            <ArrowDown />
          </Button>
          <Button
            type="button"
            variant="ghost"
            size="icon-sm"
            aria-label={t("nodes.fence.removeMethod", n)}
            onClick={onRemove}
          >
            <X />
          </Button>
        </div>
      </div>
      <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
        {row.kind === "ipmi" && (
          <>
            {text("address", t("nodes.fence.address"))}
            {text("port", t("nodes.fence.port"))}
            {text("user", t("nodes.fence.user"))}
            {password}
            <div className="flex items-center gap-2">
              <Checkbox
                id={id("lanplus")}
                checked={row.lanplus}
                onCheckedChange={(v) => onChange({ lanplus: v === true })}
              />
              <Label htmlFor={id("lanplus")}>{t("nodes.fence.lanplus")}</Label>
            </div>
          </>
        )}
        {row.kind === "redfish" && (
          <>
            {text("address", t("nodes.fence.url"), "https://bmc.example.com")}
            {text("systemId", t("nodes.fence.systemId"), t("nodes.fence.systemIdHint"))}
            {text("user", t("nodes.fence.user"))}
            {password}
            <div className="sm:col-span-2">
              <Field id={id("caPem")} label={t("nodes.fence.caPem")}>
                <textarea
                  id={id("caPem")}
                  rows={3}
                  spellCheck={false}
                  autoComplete="off"
                  value={row.caPem}
                  onChange={(e) => onChange({ caPem: e.target.value })}
                  className="w-full min-w-0 rounded-lg border border-input bg-transparent px-2.5 py-1 font-mono text-xs break-all outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50 disabled:opacity-50 dark:bg-input/30"
                />
              </Field>
            </div>
            <div className="flex items-center gap-2">
              <Checkbox
                id={id("insecure")}
                checked={row.insecure}
                onCheckedChange={(v) => onChange({ insecure: v === true })}
              />
              <Label htmlFor={id("insecure")}>{t("nodes.fence.insecure")}</Label>
            </div>
          </>
        )}
        {row.kind === "command" && (
          <>
            {text("off", t("nodes.fence.off"), t("nodes.fence.offHint"))}
            {text("status", t("nodes.fence.status"), t("nodes.fence.statusHint"))}
          </>
        )}
      </div>
    </li>
  );
}

/** `fence-test`의 결과: 방법마다 읽은 전원 */
function TestResult({ test }: { test: FenceTest }) {
  const { t } = useTranslation();
  return (
    <div className="flex flex-col gap-2">
      <p className="text-sm">{t("nodes.fence.checkedBy", { name: test.checked_by })}</p>
      <Table aria-label={t("nodes.fence.testResult")}>
        <TableHeader>
          <TableRow>
            <TableHead>#</TableHead>
            <TableHead>{t("nodes.fence.kindColumn")}</TableHead>
            <TableHead>{t("nodes.fence.power")}</TableHead>
            <TableHead>{t("nodes.fence.error")}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {test.methods.map((m, i) => (
            <TableRow key={i}>
              <TableCell>{i + 1}</TableCell>
              <TableCell>{t(`nodes.fence.kinds.${m.kind}`)}</TableCell>
              <TableCell>{t(`nodes.fence.powerState.${m.power}`)}</TableCell>
              <TableCell className="text-xs break-all text-destructive">{m.error ?? ""}</TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
    </div>
  );
}
