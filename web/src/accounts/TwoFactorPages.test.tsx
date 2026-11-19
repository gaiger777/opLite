import { act, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { AccountPage, SecurityPage } from "@/accounts/TwoFactorPages";
import { ADMIN, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

const STATUS = {
  totp: true,
  totp_pending: false,
  recovery_codes: 7,
  passkeys: [{ id: "pk1", name: "laptop", rp_id: "opl.example.com", created_at: 1700000000 }],
  required: false,
  enrolling: false,
  available: true,
};

it("regenerates recovery codes with proof and hides passkeys on IP access", async () => {
  const proofs: unknown[] = [];
  server.use(
    http.get(apiUrl("/auth/2fa"), () => HttpResponse.json(STATUS)),
    http.post(apiUrl("/auth/2fa/recovery"), async ({ request }) => {
      proofs.push(await request.json());
      return HttpResponse.json({ codes: ["1111-2222-3333-4444"] });
    }),
  );
  const user = userEvent.setup();
  renderPage(<AccountPage />);
  expect(await screen.findByText("An authenticator app is set up.")).toBeInTheDocument();
  expect(screen.getByText("7 recovery codes left.")).toBeInTheDocument();
  // rp_id가 없으면(IP 주소 접속) 패스키를 더할 수 없고 까닭을 보인다.
  expect(screen.getByText(/opened the web UI by IP address/)).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Add a passkey" })).not.toBeInTheDocument();

  await user.click(screen.getByRole("button", { name: "Make new recovery codes" }));
  const dialog = screen.getByRole("dialog", { name: "Make new recovery codes" });
  await user.type(within(dialog).getByLabelText("Or your password"), "secret-pw");
  await user.click(within(dialog).getByRole("button", { name: "Make new recovery codes" }));
  expect(await screen.findByRole("list", { name: "Recovery codes" })).toHaveTextContent(
    "1111-2222-3333-4444",
  );
  expect(proofs).toEqual([{ password: "secret-pw" }]);
});

it("saves the two-step policy for roles and realms", async () => {
  const patches: unknown[] = [];
  server.use(
    http.get(apiUrl("/settings"), () =>
      HttpResponse.json({ id: "s", name: "cluster", revision: 3, spec: {}, status: {} }),
    ),
    http.get(apiUrl("/realms"), () =>
      HttpResponse.json({
        revision: 1,
        items: [
          { name: "corp", backend: { kind: "ldap" } },
          { name: "sso", backend: { kind: "oidc" } },
        ],
      }),
    ),
    http.patch(apiUrl("/settings"), async ({ request }) => {
      patches.push(await request.json());
      return HttpResponse.json({ id: "s", name: "cluster", revision: 4, spec: {}, status: {} });
    }),
  );
  const user = userEvent.setup();
  renderPage(<SecurityPage />, { as: ADMIN });
  await user.click(await screen.findByRole("checkbox", { name: "Administrator" }));
  // OIDC 영역은 고를 수 없다.
  expect(await screen.findByRole("checkbox", { name: "corp" })).toBeInTheDocument();
  expect(screen.queryByRole("checkbox", { name: "sso" })).not.toBeInTheDocument();
  await user.click(screen.getByRole("checkbox", { name: "corp" }));
  await user.click(screen.getByRole("button", { name: "Save" }));
  await waitFor(() =>
    expect(patches).toEqual([{ two_factor: { roles: ["admin"], realms: ["corp"] } }]),
  );
});

it("shows a deleted realm left in the policy so it can be cleared", async () => {
  // 지운 영역이 정책에 남으면 서버가 저장을 400으로 막는다. 보여야 끌 수 있다(리뷰 2).
  const patches: unknown[] = [];
  server.use(
    http.get(apiUrl("/settings"), () =>
      HttpResponse.json({
        id: "s",
        name: "cluster",
        revision: 3,
        spec: { two_factor: { roles: [], realms: ["corp", "old"] } },
        status: {},
      }),
    ),
    http.get(apiUrl("/realms"), () =>
      HttpResponse.json({ revision: 1, items: [{ name: "corp", backend: { kind: "ldap" } }] }),
    ),
    http.patch(apiUrl("/settings"), async ({ request }) => {
      patches.push(await request.json());
      return HttpResponse.json({ id: "s", name: "cluster", revision: 4, spec: {}, status: {} });
    }),
  );
  const user = userEvent.setup();
  renderPage(<SecurityPage />, { as: ADMIN });
  const old = await screen.findByRole("checkbox", { name: "old (deleted)" });
  expect(old).toBeChecked();
  await user.click(old);
  await user.click(screen.getByRole("button", { name: "Save" }));
  await waitFor(() => expect(patches).toEqual([{ two_factor: { roles: [], realms: ["corp"] } }]));
});

it("asks for proof before adding an authenticator app next to a passkey", async () => {
  // 이미 요소(패스키)가 있으면 요소 더하기에 본인 확인이 필요하다(서버 `proof_required`, 리뷰 1).
  const confirms: unknown[] = [];
  let status = { ...STATUS, totp: false };
  server.use(
    http.get(apiUrl("/auth/2fa"), () => HttpResponse.json(status)),
    http.post(apiUrl("/auth/2fa/totp"), () =>
      HttpResponse.json({
        secret: "JBSWY3DPEHPK3PXP",
        uri: "otpauth://totp/x?secret=JBSWY3DPEHPK3PXP",
      }),
    ),
    http.post(apiUrl("/auth/2fa/totp/confirm"), async ({ request }) => {
      const body = (await request.json()) as { proof?: unknown };
      confirms.push(body);
      if (!body.proof) {
        return HttpResponse.json(
          { code: "proof_required", message: "proof required" },
          { status: 400 },
        );
      }
      status = { ...STATUS };
      return HttpResponse.json({ codes: [] });
    }),
  );
  const user = userEvent.setup();
  renderPage(<AccountPage />);
  await user.click(await screen.findByRole("button", { name: "Set up an authenticator app" }));
  await user.type(await screen.findByLabelText("Code from the app"), "123456");
  await user.click(screen.getByRole("button", { name: "Confirm" }));
  const dialog = await screen.findByRole("dialog", { name: "Set up an authenticator app" });
  await user.type(within(dialog).getByLabelText("Or your password"), "secret-pw");
  await user.click(within(dialog).getByRole("button", { name: "Confirm" }));
  expect(await screen.findByText("An authenticator app is set up.")).toBeInTheDocument();
  expect(confirms).toEqual([{ code: "123456", proof: { password: "secret-pw" } }]);
});

it("closes the proof dialog and clears the app code when adding an app fails", async () => {
  // 실패한 뒤 창에서 다시 누르면 지난(틀린) 앱 코드를 또 보냈다(리뷰 2). 창을 닫고 앱 코드를 비워 새로 받는다.
  const confirms: unknown[] = [];
  server.use(
    http.get(apiUrl("/auth/2fa"), () => HttpResponse.json({ ...STATUS, totp: false })),
    http.post(apiUrl("/auth/2fa/totp"), () =>
      HttpResponse.json({
        secret: "JBSWY3DPEHPK3PXP",
        uri: "otpauth://totp/x?secret=JBSWY3DPEHPK3PXP",
      }),
    ),
    http.post(apiUrl("/auth/2fa/totp/confirm"), async ({ request }) => {
      confirms.push(await request.json());
      return HttpResponse.json(
        { code: "invalid_second_factor", message: "invalid code" },
        { status: 403 },
      );
    }),
  );
  const user = userEvent.setup();
  renderPage(<AccountPage />);
  await user.click(await screen.findByRole("button", { name: "Set up an authenticator app" }));
  const code = await screen.findByLabelText("Code from the app");
  await user.type(code, "123456");
  await user.click(screen.getByRole("button", { name: "Confirm" }));
  const dialog = await screen.findByRole("dialog", { name: "Set up an authenticator app" });
  await user.type(within(dialog).getByLabelText("Code or recovery code"), "abcd-efgh");
  await user.click(within(dialog).getByRole("button", { name: "Confirm" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
  expect(await screen.findByRole("alert")).toBeInTheDocument();
  expect(screen.getByLabelText("Code from the app")).toHaveValue("");
  expect(screen.getByRole("button", { name: "Confirm" })).toBeDisabled();
  expect(confirms).toEqual([{ code: "123456", proof: { code: "abcd-efgh" } }]);
});

it("keeps unsaved policy edits when another setting changes", async () => {
  server.use(
    http.get(apiUrl("/settings"), () =>
      HttpResponse.json({ id: "s", name: "cluster", revision: 3, spec: {}, status: {} }),
    ),
    http.get(apiUrl("/realms"), () => HttpResponse.json({ revision: 1, items: [] })),
  );
  const user = userEvent.setup();
  const { queryClient } = renderPage(<SecurityPage />, { as: ADMIN });
  const admin = await screen.findByRole("checkbox", { name: "Administrator" });
  await user.click(admin);
  expect(admin).toBeChecked();
  // 다른 관리자가 원격 측정을 바꿨다(watch). 2FA 정책은 같으니 고치던 값이 남는다(리뷰 1).
  act(() =>
    queryClient.setQueryData(["settings"], {
      id: "s",
      name: "cluster",
      revision: 4,
      spec: { telemetry: { enabled: true } },
      status: {},
    }),
  );
  expect(screen.getByRole("checkbox", { name: "Administrator" })).toBeChecked();
});
