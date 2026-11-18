import { Columns3, Bookmark, X } from "lucide-react";
import { useId, useState } from "react";
import { useTranslation } from "react-i18next";

import { readViews, type TableState, type View, writeViews } from "@/components/data-table/state";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";

/** 저장 보기: 코드에 둔 기본 보기, 저장한 보기, 지금 쿼리를 이름 붙여 저장하기 */
export function ViewMenu({
  table,
  state,
  defaults,
}: {
  table: string;
  state: TableState;
  defaults: View[];
}) {
  const { t } = useTranslation();
  const [saved, setSaved] = useState(() => readViews(table));
  const [name, setName] = useState("");
  const inputId = useId();
  const store = (next: View[]) => {
    setSaved(next);
    writeViews(table, next);
  };
  const item = (view: View) => (
    <Button
      variant={view.query === state.query ? "secondary" : "ghost"}
      size="sm"
      className="flex-1 justify-start"
      aria-pressed={view.query === state.query}
      onClick={() => state.apply(view.query)}
    >
      {view.name}
    </Button>
  );
  return (
    <Popover>
      <PopoverTrigger asChild>
        <Button variant="outline" size="sm">
          <Bookmark />
          {t("table.views")}
        </Button>
      </PopoverTrigger>
      <PopoverContent align="end" className="flex w-64 flex-col gap-1">
        <ul aria-label={t("table.views")} className="flex flex-col gap-1">
          {[{ name: t("table.allRows"), query: "" }, ...defaults].map((view) => (
            <li key={`d-${view.name}`} className="flex">
              {item(view)}
            </li>
          ))}
          {saved.map((view) => (
            <li key={`s-${view.name}`} className="flex items-center gap-1">
              {item(view)}
              <Button
                variant="ghost"
                size="icon-sm"
                aria-label={t("table.deleteView", { name: view.name })}
                onClick={() => store(saved.filter((v) => v.name !== view.name))}
              >
                <X />
              </Button>
            </li>
          ))}
        </ul>
        <form
          className="mt-2 flex items-end gap-2 border-t pt-2"
          onSubmit={(e) => {
            e.preventDefault();
            const trimmed = name.trim();
            if (trimmed) {
              store([
                ...saved.filter((v) => v.name !== trimmed),
                { name: trimmed, query: state.query },
              ]);
              setName("");
            }
          }}
        >
          <div className="flex min-w-0 flex-1 flex-col gap-1">
            <Label htmlFor={inputId}>{t("table.viewName")}</Label>
            <Input id={inputId} value={name} onChange={(e) => setName(e.target.value)} />
          </div>
          <Button type="submit" size="sm" disabled={!name.trim()}>
            {t("table.saveView")}
          </Button>
        </form>
      </PopoverContent>
    </Popover>
  );
}

/** 열 고르기 */
export function ColumnMenu({
  columns,
  hidden,
  onChange,
}: {
  columns: { id: string; header: string }[];
  hidden: string[];
  onChange(hidden: string[]): void;
}) {
  const { t } = useTranslation();
  const baseId = useId();
  return (
    <Popover>
      <PopoverTrigger asChild>
        <Button variant="outline" size="sm">
          <Columns3 />
          {t("table.columns")}
        </Button>
      </PopoverTrigger>
      <PopoverContent align="end" className="flex w-56 flex-col gap-2">
        <p className="text-sm font-medium">{t("table.columns")}</p>
        {columns.map((c) => {
          const id = `${baseId}-${c.id}`;
          return (
            <div key={c.id} className="flex items-center gap-2">
              <Checkbox
                id={id}
                checked={!hidden.includes(c.id)}
                onCheckedChange={(v) =>
                  onChange(v === true ? hidden.filter((h) => h !== c.id) : [...hidden, c.id])
                }
              />
              <Label htmlFor={id}>{c.header}</Label>
            </div>
          );
        })}
      </PopoverContent>
    </Popover>
  );
}
