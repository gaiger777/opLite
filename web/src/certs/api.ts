// ACME 인증서(계획 73 §11)의 질의와 순수 도움 함수. 세 목록은 가린 보기(계정 키·EAB HMAC·DNS 비밀·공유 키가 없다)이고
// watch 사건이 오면 다시 읽는다(`LiveProvider`의 `VIEW_KINDS`).
import { type QueryClient, useQuery } from "@tanstack/react-query";

import { api, call } from "@/api/client";
import type { components } from "@/api/schema";
import type { Node } from "@/api/types";

type Schemas = components["schemas"];
export type AcmeAccount = Schemas["Resource_AcmeAccountSpec_AcmeAccountStatus"];
export type AcmeDns = Schemas["Resource_AcmeDnsSpec_AcmeDnsStatus"];
export type Certificate = Schemas["Resource_CertificateSpec_CertificateStatus"];
export type IssuedCert = Schemas["IssuedCert"];
export type DnsProvider = Schemas["DnsProvider"];
export type DnsKind = DnsProvider["kind"];
export type CreateAccountRequest = Schemas["CreateAccountRequest"];
export type CreateDnsRequest = Schemas["CreateDnsRequest"];
export type CreateCertificateRequest = Schemas["CreateCertificateRequest"];

export const ACCOUNTS_KEY = ["acme-accounts"] as const;
export const DNS_KEY = ["acme-dns"] as const;
export const CERTIFICATES_KEY = ["certificates"] as const;

export function useAccounts(enabled = true) {
  return useQuery({
    queryKey: ACCOUNTS_KEY,
    queryFn: () => call(api.GET("/acme-accounts")),
    select: (list) => list.items,
    enabled,
  });
}

export function useDnsProviders(enabled = true) {
  return useQuery({
    queryKey: DNS_KEY,
    queryFn: () => call(api.GET("/acme-dns")),
    select: (list) => list.items,
    enabled,
  });
}

export function useCertificates(enabled = true) {
  return useQuery({
    queryKey: CERTIFICATES_KEY,
    queryFn: () => call(api.GET("/certificates")),
    select: (list) => list.items,
    enabled,
  });
}

export async function refreshCerts(queryClient: QueryClient) {
  await Promise.all(
    [ACCOUNTS_KEY, DNS_KEY, CERTIFICATES_KEY].map((queryKey) =>
      queryClient.invalidateQueries({ queryKey }),
    ),
  );
}

/** 디렉터리 별명(서버 `acme::directory_url`) — 나머지는 URL을 적는다 */
export const DIRECTORIES = ["letsencrypt", "letsencrypt-staging", "zerossl", "google"] as const;

/** DNS 공급자 종류의 차례 */
export const DNS_KINDS: readonly DnsKind[] = [
  "cloudflare",
  "route53",
  "rfc2136",
  "power_dns",
  "hook",
];

/** HTTP-01 노드 기능(서버 `opl_types::node::HTTP01` — `[api] http_listen`을 켠 노드가 알린다) */
export const HTTP01 = "http01";

/**
 * HTTP-01 인증서가 덮는 노드 가운데 포트 80 리스너가 없는 노드 이름(서버 `covered_nodes`와 같은 셈 — 위트니스 빼고, 노드마다면
 * 고른 노드만, 비었으면 모두). 있으면 서버가 400 `http01_unavailable`로 거절하므로 대화상자가 먼저 알린다.
 */
export function http01Missing(nodes: readonly Node[], perNode: boolean, chosen: readonly string[]) {
  return nodes
    .filter((n) => !n.spec.witness)
    .filter((n) => !perNode || chosen.length === 0 || chosen.includes(n.id))
    .filter((n) => !(n.status.features ?? []).includes(HTTP01))
    .map((n) => n.name)
    .sort();
}

/** 이름 칸(쉼표·줄바꿈으로 나눔)을 이름 목록으로 */
export function splitNames(text: string): string[] {
  return text
    .split(/[\s,]+/)
    .map((s) => s.trim())
    .filter(Boolean);
}

/** 받은 인증서들: 공유면 `shared` 하나, 노드마다면 노드 id → 인증서 */
export function issued(cert: Certificate): { slot: string; cert: IssuedCert }[] {
  const out: { slot: string; cert: IssuedCert }[] = [];
  if (cert.status.shared) {
    out.push({ slot: "shared", cert: cert.status.shared });
  }
  for (const [node, c] of Object.entries(cert.status.nodes ?? {})) {
    out.push({ slot: node, cert: c });
  }
  return out;
}

/** 남은 날(유닉스 초 기준 — 음수면 지남) */
export function daysLeft(notAfter: number, nowMs = Date.now()): number {
  return Math.floor((notAfter * 1000 - nowMs) / 86_400_000);
}

/** 만료 칩의 색: 지남·7일 밑 빨강, 21일 밑 노랑, 나머지 초록 */
export function expiryTone(days: number): "danger" | "warning" | "success" {
  return days < 7 ? "danger" : days < 21 ? "warning" : "success";
}
