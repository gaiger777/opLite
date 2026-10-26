import {
  type ColumnDef,
  flexRender,
  getCoreRowModel,
  getFilteredRowModel,
  getSortedRowModel,
  type RowSelectionState,
  type SortingState,
  useReactTable,
} from "@tanstack/react-table";
import { ArrowDown, ArrowUp, ArrowUpDown, Plus } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";

import { useList } from "@/api/live";
import type { Vm } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { TableMessage } from "@/components/TableMessage";
import { Input } from "@/components/ui/input";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { formatMib } from "@/lib/format";
import { readPref, writePref } from "@/lib/prefs";
import { useVmActions } from "@/vms/actions";
import { VmMenu } from "@/vms/VmMenu";
import { VmPhaseBadge } from "@/vms/VmPhaseBadge";

/** 정렬할 수 있는 열 */
const SORTABLE = new Set(["name", "phase", "node", "cpu", "memory"]);

/** 저장해 둔 정렬. 모양이 다르면(손으로 고쳤거나 열이 바뀌었으면) 버린다. */
function savedSorting(): SortingState {
  try {
    const value: unknown = JSON.parse(readPref("vms.sort") ?? "[]");
    const valid =
      Array.isArray(value) &&
      value.every(
        (s: unknown) =>
          typeof s === "object" &&
          s !== null &&
          "id" in s &&
          typeof s.id === "string" &&
          SORTABLE.has(s.id) &&
          "desc" in s &&
          typeof s.desc === "boolean",
      );
    return valid ? (value as SortingState) : [];
  } catch {
    return [];
  }
}

export function vcpus(vm: Vm): number {
  return (vm.spec.cpu.sockets ?? 1) * vm.spec.cpu.cores;
}

export function VmListPage() {
  const { t } = useTranslation();
  const me = usePrincipal();
  const vms = useList("vm");
  const nodes = useList("node");
  const { runMany } = useVmActions();
  const [filter, setFilter] = useState("");
  const [sorting, setSorting] = useState<SortingState>(savedSorting);
  const [selection, setSelection] = useState<RowSelectionState>({});

  useEffect(() => writePref("vms.sort", JSON.stringify(sorting)), [sorting]);

  const nodeName = useMemo(() => {
    const names = new Map(nodes.data?.map((n) => [n.id, n.name]));
    return (id: string | null | undefined) => (id ? (names.get(id) ?? id.slice(-8)) : "—");
  }, [nodes.data]);

  const columns = useMemo<ColumnDef<Vm>[]>(
    () => [
      {
        id: "select",
        enableSorting: false,
        header: ({ table }) => (
          <Checkbox
            aria-label={t("vms.selectAll")}
            checked={
              table.getIsAllRowsSelected() || (table.getIsSomeRowsSelected() && "indeterminate")
            }
            onCheckedChange={(v) => table.toggleAllRowsSelected(v === true)}
          />
        ),
        cell: ({ row }) => (
          <Checkbox
            aria-label={t("vms.select", { name: row.original.name })}
            checked={row.getIsSelected()}
            onCheckedChange={(v) => row.toggleSelected(v === true)}
          />
        ),
      },
      {
        id: "name",
        accessorKey: "name",
        header: t("vms.name"),
        cell: ({ row }) => (
          <Link
            to={`/vms/${encodeURIComponent(row.original.name)}`}
            className="font-medium underline-offset-4 hover:underline"
          >
            {row.original.name}
          </Link>
        ),
      },
      {
        id: "phase",
        accessorFn: (vm) => vm.status.phase,
        header: t("vms.state"),
        cell: ({ row }) => <VmPhaseBadge vm={row.original} />,
      },
      {
        id: "node",
        accessorFn: (vm) => nodeName(vm.spec.node ?? vm.status.node),
        header: t("vms.node"),
      },
      { id: "cpu", accessorFn: vcpus, header: t("vms.vcpus") },
      {
        id: "memory",
        accessorFn: (vm) => vm.spec.memory_mib,
        header: t("vms.memory"),
        cell: ({ row }) => formatMib(row.original.spec.memory_mib),
      },
      {
        id: "menu",
        enableSorting: false,
        header: () => <span className="sr-only">{t("vms.actions")}</span>,
        cell: ({ row }) => <VmMenu vm={row.original} />,
      },
    ],
    [t, nodeName],
  );

  const table = useReactTable({
    data: vms.data ?? [],
    columns,
    state: { sorting, rowSelection: selection, globalFilter: filter },
    getRowId: (vm) => vm.id,
    onSortingChange: setSorting,
    onRowSelectionChange: setSelection,
    globalFilterFn: (row, _column, value: string) =>
      row.original.name.toLowerCase().includes(value.toLowerCase()),
    getCoreRowModel: getCoreRowModel(),
    getSortedRowModel: getSortedRowModel(),
    getFilteredRowModel: getFilteredRowModel(),
  });

  // 검색으로 가려진 행은 선택돼 있어도 일괄 동작에 넣지 않는다.
  const selected = table.getFilteredSelectedRowModel().rows.map((r) => r.original);
  const bulk = (action: "start" | "stop") => {
    void runMany(selected, action);
    setSelection({});
  };

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h1 className="text-2xl font-semibold">{t("nav.vms")}</h1>
        <div className="flex items-center gap-2">
          <Input
            className="w-48"
            placeholder={t("vms.search")}
            aria-label={t("vms.search")}
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
          />
          {can(me.role, "write", "vm") && (
            <Button asChild>
              <Link to="/vms/-new">
                <Plus />
                {t("vms.create")}
              </Link>
            </Button>
          )}
        </div>
      </div>
      {selected.length > 0 && can(me.role, "operate", "vm") && (
        <div className="flex items-center gap-2 text-sm">
          <span>{t("vms.selected", { count: selected.length })}</span>
          <Button size="sm" variant="outline" onClick={() => bulk("start")}>
            {t("vms.action.start")}
          </Button>
          <Button size="sm" variant="outline" onClick={() => bulk("stop")}>
            {t("vms.action.stop")}
          </Button>
        </div>
      )}
      <Table>
        <TableHeader>
          {table.getHeaderGroups().map((group) => (
            <TableRow key={group.id}>
              {group.headers.map((header) => {
                const sorted = header.column.getIsSorted();
                const SortIcon =
                  sorted === "asc" ? ArrowUp : sorted === "desc" ? ArrowDown : ArrowUpDown;
                return (
                  <TableHead
                    key={header.id}
                    aria-sort={
                      !header.column.getCanSort()
                        ? undefined
                        : sorted === "asc"
                          ? "ascending"
                          : sorted === "desc"
                            ? "descending"
                            : "none"
                    }
                  >
                    {header.column.getCanSort() ? (
                      <button
                        type="button"
                        className="inline-flex items-center gap-1"
                        onClick={header.column.getToggleSortingHandler()}
                      >
                        {flexRender(header.column.columnDef.header, header.getContext())}
                        <SortIcon aria-hidden className={sorted ? "size-3" : "size-3 opacity-50"} />
                      </button>
                    ) : (
                      flexRender(header.column.columnDef.header, header.getContext())
                    )}
                  </TableHead>
                );
              })}
            </TableRow>
          ))}
        </TableHeader>
        <TableBody>
          {table.getRowModel().rows.length === 0 && (
            <TableMessage colSpan={columns.length} query={vms} empty={t("vms.empty")} />
          )}
          {table.getRowModel().rows.map((row) => (
            <TableRow key={row.id} data-state={row.getIsSelected() ? "selected" : undefined}>
              {row.getVisibleCells().map((cell) => (
                <TableCell key={cell.id}>
                  {flexRender(cell.column.columnDef.cell, cell.getContext())}
                </TableCell>
              ))}
            </TableRow>
          ))}
        </TableBody>
      </Table>
    </div>
  );
}
