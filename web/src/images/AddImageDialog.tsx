import { useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useTranslation } from "react-i18next";

import { api, call } from "@/api/client";
import type { components } from "@/api/schema";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { Input } from "@/components/ui/input";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { type Arch, ARCHES, type CatalogImage, digestError, validCatalogId } from "@/images/images";
import { urlError } from "@/isos/isos";
import { gibToBytes } from "@/vms/form";

type Schemas = components["schemas"];
type ChecksumKind = Schemas["Checksum"]["kind"];

/**
 * 관리자 카탈로그 항목 더하기(`POST /image-catalog`). id는 아키텍처마다 같은 것을 쓰고 내장 항목의 id는 쓸 수 없다. 체크섬은
 * 고정 해시(SHA-256·SHA-512)나 체크섬 파일 URL(`SHA256SUMS` 모양) 가운데 하나다.
 */
export function AddImageDialog({ onClose }: { onClose(): void }) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const [id, setId] = useState("");
  const [title, setTitle] = useState("");
  const [os, setOs] = useState("");
  const [version, setVersion] = useState("");
  const [arch, setArch] = useState<Arch>("x86_64");
  const [url, setUrl] = useState("");
  const [kind, setKind] = useState<ChecksumKind>("sums");
  const [checksum, setChecksum] = useState("");
  const [sourceFormat, setSourceFormat] = useState<"qcow2" | "raw">("qcow2");
  const [firmware, setFirmware] = useState<Schemas["Firmware"]>("uefi");
  const [defaultUser, setDefaultUser] = useState("");
  const [sizeGib, setSizeGib] = useState("");
  const [errors, setErrors] = useState<Record<string, string>>({});

  async function submit() {
    const found: Record<string, string> = {};
    if (!validCatalogId(id)) {
      found.id = "images.errors.id";
    }
    for (const [field, value] of [
      ["title", title],
      ["os", os],
      ["version", version],
    ] as const) {
      if (value.trim() === "") {
        found[field] = "images.errors.required";
      }
    }
    const badUrl = urlError(url.trim());
    if (badUrl) {
      found.url = badUrl;
    }
    const sum = checksum.trim();
    const badSum = kind === "sums" ? urlError(sum) : digestError(kind, sum);
    if (badSum) {
      found.checksum = badSum;
    }
    if (arch === "aarch64" && firmware === "bios") {
      found.firmware = "images.errors.aarch64Bios";
    }
    const size = sizeGib.trim() === "" ? 0 : gibToBytes(sizeGib.trim());
    if (size === undefined) {
      found.size = "vmForm.errors.size";
    }
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    const image: CatalogImage = {
      id,
      title: title.trim(),
      os: os.trim(),
      version: version.trim(),
      arch,
      url: url.trim(),
      checksum: kind === "sums" ? { kind, url: sum } : { kind, value: sum.toLowerCase() },
      source_format: sourceFormat,
      firmware,
      ...(defaultUser.trim() !== "" && { default_user: defaultUser.trim() }),
      ...(size ? { min_size_bytes: size } : {}),
    };
    await call(api.POST("/image-catalog", { body: image }));
    await queryClient.invalidateQueries({ queryKey: ["image-catalog"] });
  }

  const text = (
    field: string,
    label: string,
    value: string,
    set: (v: string) => void,
    extra: { hint?: string; mono?: boolean; type?: string } = {},
  ) => (
    <Field id={`catalog-${field}`} label={label} error={errors[field]} help={extra.hint}>
      {(a11y) => (
        <Input
          id={`catalog-${field}`}
          type={extra.type}
          className={extra.mono ? "font-mono" : undefined}
          value={value}
          onChange={(e) => set(e.target.value)}
          {...a11y}
        />
      )}
    </Field>
  );

  return (
    <FormDialog
      title={t("images.addTitle")}
      description={t("images.addDescription")}
      submitLabel={t("images.add")}
      className="sm:max-w-lg"
      onClose={onClose}
      onSubmit={submit}
    >
      <div className="-mx-1 flex max-h-[65svh] flex-col gap-4 overflow-y-auto px-1">
        {text("id", t("images.id"), id, setId, { hint: t("images.idHint"), mono: true })}
        {text("title", t("images.title"), title, setTitle, { hint: t("help.images.add.title") })}
        <div className="grid grid-cols-2 gap-4">
          {text("os", t("images.os"), os, setOs, { hint: t("help.images.add.os") })}
          {text("version", t("images.version"), version, setVersion, {
            hint: t("help.images.add.version"),
          })}
        </div>
        {text("url", t("downloads.url"), url, setUrl, {
          type: "url",
          hint: t("help.images.add.url"),
        })}
        <div className="grid gap-4 sm:grid-cols-3">
          <Field id="catalog-arch" label={t("images.arch")} help={t("help.images.add.arch")}>
            {(a11y) => (
              <NativeSelect
                id="catalog-arch"
                className="w-full"
                value={arch}
                onChange={(e) => setArch(e.target.value as Arch)}
                {...a11y}
              >
                {ARCHES.map((a) => (
                  <NativeSelectOption key={a} value={a}>
                    {a}
                  </NativeSelectOption>
                ))}
              </NativeSelect>
            )}
          </Field>
          <Field
            id="catalog-firmware"
            label={t("vms.firmware")}
            error={errors.firmware}
            help={t("help.images.add.firmware")}
          >
            {(a11y) => (
              <NativeSelect
                id="catalog-firmware"
                className="w-full"
                value={firmware}
                onChange={(e) => setFirmware(e.target.value as typeof firmware)}
                {...a11y}
              >
                <NativeSelectOption value="uefi">UEFI</NativeSelectOption>
                <NativeSelectOption value="bios">BIOS</NativeSelectOption>
              </NativeSelect>
            )}
          </Field>
          <Field
            id="catalog-format"
            label={t("uploads.sourceFormat")}
            help={t("help.images.add.format")}
          >
            {(a11y) => (
              <NativeSelect
                id="catalog-format"
                className="w-full"
                value={sourceFormat}
                onChange={(e) => setSourceFormat(e.target.value as typeof sourceFormat)}
                {...a11y}
              >
                <NativeSelectOption value="qcow2">qcow2</NativeSelectOption>
                <NativeSelectOption value="raw">raw</NativeSelectOption>
              </NativeSelect>
            )}
          </Field>
        </div>
        <Field
          id="catalog-kind"
          label={t("images.checksum")}
          help={t("help.images.add.checksumKind")}
          info="images.checksum"
        >
          {(a11y) => (
            <NativeSelect
              id="catalog-kind"
              className="w-full"
              value={kind}
              onChange={(e) => setKind(e.target.value as ChecksumKind)}
              {...a11y}
            >
              <NativeSelectOption value="sums">{t("images.checksums.sums")}</NativeSelectOption>
              <NativeSelectOption value="sha256">{t("images.checksums.sha256")}</NativeSelectOption>
              <NativeSelectOption value="sha512">{t("images.checksums.sha512")}</NativeSelectOption>
            </NativeSelect>
          )}
        </Field>
        {text(
          "checksum",
          kind === "sums" ? t("images.sumsUrl") : t("images.digest"),
          checksum,
          setChecksum,
          {
            mono: kind !== "sums",
            hint: t(kind === "sums" ? "help.images.add.sumsUrl" : "help.images.add.digest"),
          },
        )}
        <div className="grid grid-cols-2 gap-4">
          {text("user", t("images.defaultUser"), defaultUser, setDefaultUser, {
            hint: t("help.images.add.defaultUser"),
          })}
          {text("size", t("images.minSize"), sizeGib, setSizeGib, {
            hint: t("help.images.add.minSize"),
          })}
        </div>
      </div>
    </FormDialog>
  );
}
