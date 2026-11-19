import { X } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";

import type { components } from "@/api/schema";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { flagProblem, splitFlag, toFlag, useCpuFlags } from "@/cpu/cpu";

/**
 * CPU 플래그 칩 입력(설계 §13). `signed`면 VM 플래그(`-avx512vbmi` 숨기기·`+aes` 켜기, 칩을 눌러 바꿈)이고, 아니면
 * 숨김 목록(이름만)이다. 이름은 `/cpu/flags`(노드 QEMU가 아는 이름)로 자동 완성하고 모르는 이름은 받지 않는다(목록을
 * 읽지 못하면 서버가 본다). 입력의 기본은 숨기기다. 루트 `cluster.admin`이 아니면(`turn_on_restricted`가 거짓) 켜기는
 * 허용 목록(`restricted`가 아닌 것)만이고 그 목록과 까닭을 보인다(리뷰 2 #3). 켜면 라이브 마이그레이션을 막는 플래그는
 * 칩에 적는다.
 */
export function CpuFlagsInput({
  id,
  label,
  hint,
  flags,
  onChange,
  arches,
  signed,
  disabled = false,
}: {
  id: string;
  label: string;
  hint?: string;
  flags: string[];
  onChange(flags: string[]): void;
  /** 이름을 찾을 아키텍처(숨김 목록은 모든 아키텍처). 비면 서버가 노드들의 아키텍처로 고른다. */
  arches: (components["schemas"]["Arch"] | undefined)[];
  signed: boolean;
  disabled?: boolean;
}) {
  const { t } = useTranslation();
  const [text, setText] = useState("");
  const [problem, setProblem] = useState<{ key: string; name?: string } | null>(null);
  const first = useCpuFlags(arches[0], arches.length > 0);
  const second = useCpuFlags(arches[1], arches.length > 1);
  const lists = [first, second].slice(0, arches.length);
  const entries = lists.flatMap((q) => q.data?.flags ?? []);
  // 모두 읽었을 때만 이름을 거른다.
  const known = lists.every((q) => q.isSuccess) ? new Set(entries.map((f) => f.name)) : undefined;
  const masked = new Set(entries.filter((f) => f.masked).map((f) => f.name));
  // 이 사용자가 켤 수 없는 이름(읽지 못하면 서버가 본다)
  const limited = signed && lists.some((q) => q.data?.turn_on_restricted === false);
  const restricted = new Set(limited ? entries.filter((f) => f.restricted).map((f) => f.name) : []);
  const allowed = [...new Set(entries.filter((f) => !f.restricted).map((f) => f.name))].sort();
  const blocksMigration = new Set(entries.filter((f) => f.blocks_migration).map((f) => f.name));
  // 두 아키텍처가 같이 아는 이름은 한 번만 제안한다. 켤 수 있는 이름은 `+이름`으로도 제안한다.
  const suggestions = [
    ...new Set(entries.map((f) => f.name)),
    ...(limited ? allowed.map((name) => `+${name}`) : []),
  ];
  const signedFlags = signed ? flags : flags.map((f) => `-${f}`);
  const listId = `${id}-names`;
  const errorId = `${id}-error`;
  const hintId = `${id}-hint`;

  /** `fallback`은 부호를 적지 않았을 때다(Enter·"숨기기"는 숨기기, "켜기"는 켜기). 적은 `+`·`-`가 늘 이긴다. */
  function add(fallback: boolean) {
    const found = flagProblem(text, signedFlags, known);
    setProblem(found === null ? null : { key: found });
    if (found !== null) {
      return;
    }
    const typed = /^[+-]/.test(text.trim());
    const flag = toFlag(text, signed ? (typed ? undefined : fallback) : false);
    const { on, name } = splitFlag(flag);
    if (on && restricted.has(name)) {
      setProblem({ key: "cpu.flags.restricted", name });
      return;
    }
    onChange([...flags, signed ? flag : splitFlag(flag).name]);
    setText("");
  }

  function toggle(index: number) {
    const { on, name } = splitFlag(flags[index]!);
    if (!on && restricted.has(name)) {
      return;
    }
    onChange(flags.map((f, i) => (i === index ? `${on ? "-" : "+"}${name}` : f)));
  }

  return (
    <div className="flex flex-col gap-1.5 sm:col-span-2">
      <Label htmlFor={id}>{label}</Label>
      {flags.length > 0 && (
        <ul aria-label={label} className="flex flex-wrap gap-1.5">
          {flags.map((flag, i) => {
            const { on, name } = signed ? splitFlag(flag) : { on: false, name: flag };
            const unknown = known !== undefined && !known.has(name);
            const blocked = signed && on && masked.has(name);
            // 숨긴 제한 플래그는 켜기로 바꾸지 못한다(빼기는 된다).
            const locked = signed && !on && restricted.has(name);
            const toggleLabel = locked
              ? t("cpu.flags.restrictedToggle", { name })
              : t(on ? "cpu.flags.onHide" : "cpu.flags.offShow", { name });
            return (
              <li
                key={name}
                className={
                  "flex items-center gap-1 rounded-md border px-1.5 py-0.5 font-mono text-xs " +
                  (unknown || blocked ? "border-destructive text-destructive" : "")
                }
              >
                {signed ? (
                  <button
                    type="button"
                    disabled={disabled || locked}
                    className="underline-offset-2 hover:underline disabled:no-underline"
                    aria-label={toggleLabel}
                    title={toggleLabel}
                    onClick={() => toggle(i)}
                  >
                    {on ? "+" : "−"}
                    {name}
                  </button>
                ) : (
                  <span>{name}</span>
                )}
                {unknown && <span className="font-sans">({t("cpu.flags.unknown")})</span>}
                {blocked && <span className="font-sans">({t("cpu.flags.masked")})</span>}
                {signed && on && blocksMigration.has(name) && (
                  <span className="font-sans text-warning-fg">
                    ({t("cpu.flags.blocksMigration")})
                  </span>
                )}
                <button
                  type="button"
                  disabled={disabled}
                  aria-label={t("cpu.flags.remove", { name })}
                  className="text-muted-foreground hover:text-foreground"
                  onClick={() => onChange(flags.filter((_, j) => j !== i))}
                >
                  <X className="size-3" />
                </button>
              </li>
            );
          })}
        </ul>
      )}
      <div className="flex flex-wrap gap-2">
        <Input
          id={id}
          className="w-56 font-mono"
          list={listId}
          value={text}
          disabled={disabled}
          autoComplete="off"
          placeholder="avx512vbmi"
          aria-invalid={problem ? true : undefined}
          aria-describedby={problem ? errorId : hint || limited ? hintId : undefined}
          onChange={(e) => {
            setText(e.target.value);
            setProblem(null);
          }}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              add(false);
            }
          }}
        />
        <datalist id={listId}>
          {suggestions.map((name) => (
            <option key={name} value={name} />
          ))}
        </datalist>
        <Button
          type="button"
          variant="outline"
          size="sm"
          disabled={disabled || text.trim() === ""}
          onClick={() => add(false)}
        >
          {t("cpu.flags.hide")}
        </Button>
        {signed && (
          <Button
            type="button"
            variant="ghost"
            size="sm"
            disabled={disabled || text.trim() === ""}
            onClick={() => add(true)}
          >
            {t("cpu.flags.show")}
          </Button>
        )}
      </div>
      {problem ? (
        <p id={errorId} className="text-xs text-destructive">
          {t(problem.key, { name: problem.name })}
        </p>
      ) : (
        (hint || limited) && (
          <div id={hintId} className="flex flex-col gap-1 text-xs text-muted-foreground">
            {hint && <p>{hint}</p>}
            {limited && <p>{t("cpu.flags.allowedOnly", { flags: allowed.join(", ") })}</p>}
          </div>
        )
      )}
    </div>
  );
}
