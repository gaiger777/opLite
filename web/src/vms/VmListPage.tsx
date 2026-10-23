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
import { ArrowUpDown, Plus } from "lucide-react";
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";

import { useList } from "@/api/live";
import type { Vm } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { errorMessage } from "@/i18n/errors";
import { formatMib } from "@/lib/format";
import { readPref, writePref } from "@/lib/prefs";
import { canRun, useVmActions } from "@/vms/actions";
import { VmMenu } from "@/vms/VmMenu";
import { VmPhaseBadge } from "@/vms/VmPhaseBadge";

function savedSorting(): SortingState {
  try {
    return JSON.parse(readPref("vms.sort") ?? "[]") as SortingState;
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
  const { run } = useVmActions();
  const [filter, setFilter] = useState("");
  const [sorting, setSorting] = useState<SortingState>(savedSorting);
  const [selection, setSelection] = useState<RowSelectionState>({});

  const nodeName = useMemo(() => {
    const names = new Map(nodes.data?.map((n) => [n.id, n.name]));
    return (id: string | null | undefined) => (id ? (names.get(id) ?? id.slice(0, 8)) : "");
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
        accessorFn: (vm) => nodeName(vm.status.node ?? vm.spec.node),
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
    onSortingChange: (updater) => {
      setSorting((old) => {
        const next = typeof updater === "function" ? updater(old) : updater;
        writePref("vms.sort", JSON.stringify(next));
        return next;
      });
    },
    onRowSelectionChange: setSelection,
    globalFilterFn: (row, _column, value: string) =>
      row.original.name.toLowerCase().includes(value.toLowerCase()),
    getCoreRowModel: getCoreRowModel(),
    getSortedRowModel: getSortedRowModel(),
    getFilteredRowModel: getFilteredRowModel(),
  });

  const selected = table.getSelectedRowModel().rows.map((r) => r.original);
  const bulk = (action: "start" | "stop") => {
    for (const vm of selected.filter((vm) => canRun(vm, action))) {
      void run(vm, action);
    }
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
              <Link to="/vms/new">
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
              {group.headers.map((header) => (
                <TableHead key={header.id}>
                  {header.column.getCanSort() ? (
                    <button
                      type="button"
                      className="inline-flex items-center gap-1"
                      onClick={header.column.getToggleSortingHandler()}
                    >
                      {flexRender(header.column.columnDef.header, header.getContext())}
                      <ArrowUpDown className="size-3 opacity-50" />
                    </button>
                  ) : (
                    flexRender(header.column.columnDef.header, header.getContext())
                  )}
                </TableHead>
              ))}
            </TableRow>
          ))}
        </TableHeader>
        <TableBody>
          {table.getRowModel().rows.length === 0 && (
            <TableRow>
              <TableCell colSpan={columns.length} className="text-center text-muted-foreground">
                {vms.isPending
                  ? t("common.loading")
                  : vms.isError
                    ? errorMessage(t, vms.error)
                    : t("vms.empty")}
              </TableCell>
            </TableRow>
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
