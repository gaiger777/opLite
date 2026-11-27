import { changeSummary, fieldName, kindName, valueText } from "@/history/describe";
import i18n from "@/i18n";
import { entry } from "@/test/declarative";

const ko = i18n.getFixedT("ko");
const en = i18n.getFixedT("en");

it("names kinds in plain words, from store and manifest forms", () => {
  expect(kindName("notification-target", ko)).toBe("알림 대상");
  expect(kindName("NotificationTarget", ko)).toBe("알림 대상");
  expect(kindName("Vm", en)).toBe("VM");
  expect(kindName("brand-new", en)).toBe("brand-new");
});

it("names fields by path, then by their last part, else keeps the path", () => {
  expect(fieldName("memory_mib", ko)).toEqual({ label: "메모리", raw: false });
  expect(fieldName("cpu.cores", en)).toEqual({ label: "CPU cores", raw: false });
  expect(fieldName("ha.max_restart", en)).toEqual({ label: "max restarts", raw: false });
  expect(fieldName("labels.tier", ko)).toEqual({ label: "레이블 tier", raw: false });
  expect(fieldName("zzz.qqq", en)).toEqual({ label: "zzz.qqq", raw: true });
});

it("shows sizes with units, booleans as on/off and secrets as marks", () => {
  expect(valueText("memory_mib", 8192, ko)).toBe("8 GiB");
  expect(valueText("memory_mib", 1536, ko)).toBe("1536 MiB");
  expect(valueText("size_bytes", 1073741824, en)).toBe("1 GiB");
  expect(valueText("tpm", true, ko)).toBe("켬");
  expect(valueText("p", { $secret: "changed" }, en)).toBe("(secret changed)");
  expect(valueText("p", null, en)).toBeNull();
});

it("sums up a change: before → after where the server sent values, else the field name", () => {
  const row = entry(1, {
    changed: ["memory_mib", "description", "nics", "disks", "tpm"],
    values: [
      { path: "memory_mib", op: "replace", old: 4096, new: 8192 },
      { path: "description", op: "remove", old: "x" },
    ],
  });
  expect(changeSummary(row, ko)).toEqual([
    "메모리 4 GiB → 8 GiB",
    "설명 지움",
    "네트워크 카드(NIC) 바뀜",
    "외 2칸",
  ]);
  expect(changeSummary(entry(2, { op: "create", changed: [] }), ko)).toEqual([]);
});
