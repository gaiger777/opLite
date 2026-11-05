import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { QueryClient } from "@tanstack/react-query";
import { http, HttpResponse } from "msw";

import { SETTINGS_KEY } from "@/api/live";

import { SettingsPage } from "@/settings/SettingsPage";
import { ADMIN, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

// 아직 저장한 적 없는 설정: 서버가 기본값을 revision 0으로 돌려준다.
const DEFAULT = {
  id: "00000000-0000-0000-0000-000000000000",
  name: "cluster",
  revision: 0,
  labels: {},
  spec: { updates: { channel: "stable" } },
  status: {},
};

it("changes the update channel with the revision it read", async () => {
  const sent: { ifMatch: string | null; body: unknown }[] = [];
  server.use(
    http.get(apiUrl("/settings"), () => HttpResponse.json(DEFAULT)),
    http.patch(apiUrl("/settings"), async ({ request }) => {
      const body = (await request.json()) as { updates: { channel: string } };
      sent.push({ ifMatch: request.headers.get("If-Match"), body });
      return HttpResponse.json({ ...DEFAULT, revision: 12, spec: { updates: body.updates } });
    }),
  );
  renderPage(<SettingsPage />);
  const channel = await screen.findByLabelText("Update channel");
  expect(channel).toHaveValue("stable");
  const save = screen.getByRole("button", { name: "Save" });
  // 바뀐 것이 없으면 보낼 것도 없다.
  expect(save).toBeDisabled();
  await userEvent.selectOptions(channel, "testing");
  await userEvent.click(save);
  expect(await screen.findByText("Settings saved.")).toBeInTheDocument();
  expect(sent).toEqual([{ ifMatch: "0", body: { updates: { channel: "testing" } } }]);
  // 응답을 바로 보여 준다.
  expect(screen.getByLabelText("Update channel")).toHaveValue("testing");
  expect(screen.getByRole("button", { name: "Save" })).toBeDisabled();
});

it("keeps newer settings that arrived while saving", async () => {
  const page: { queryClient?: QueryClient } = {};
  server.use(
    http.get(apiUrl("/settings"), () => HttpResponse.json(DEFAULT)),
    http.patch(apiUrl("/settings"), () => {
      // 응답 전에 watch가 더 새 설정(다른 admin이 stable로 되돌림)을 넣었다.
      page.queryClient!.setQueryData(SETTINGS_KEY, { ...DEFAULT, revision: 20 });
      return HttpResponse.json({
        ...DEFAULT,
        revision: 12,
        spec: { updates: { channel: "testing" } },
      });
    }),
  );
  page.queryClient = renderPage(<SettingsPage />).queryClient;
  const channel = await screen.findByLabelText("Update channel");
  await userEvent.selectOptions(channel, "testing");
  await userEvent.click(screen.getByRole("button", { name: "Save" }));
  await vi.waitFor(() => expect(channel).toHaveValue("stable"));
  expect(page.queryClient.getQueryData(SETTINGS_KEY)).toMatchObject({ revision: 20 });
});

it("says why the settings could not be saved", async () => {
  server.use(
    http.get(apiUrl("/settings"), () => HttpResponse.json(DEFAULT)),
    http.patch(apiUrl("/settings"), () =>
      HttpResponse.json({ code: "revision_mismatch", message: "stale" }, { status: 412 }),
    ),
  );
  renderPage(<SettingsPage />);
  await userEvent.selectOptions(await screen.findByLabelText("Update channel"), "testing");
  await userEvent.click(screen.getByRole("button", { name: "Save" }));
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "Someone else changed it first. Reload and try again.",
  );
});

it("shows the settings read-only to everyone but admins", async () => {
  server.use(
    http.get(apiUrl("/settings"), () =>
      HttpResponse.json({ ...DEFAULT, revision: 3, spec: { updates: { channel: "testing" } } }),
    ),
  );
  renderPage(<SettingsPage />, { as: { ...ADMIN, role: "viewer" } });
  const channel = await screen.findByLabelText("Update channel");
  expect(channel).toHaveValue("testing");
  expect(channel).toBeDisabled();
  expect(screen.queryByRole("button", { name: "Save" })).not.toBeInTheDocument();
  expect(screen.getByText("Only administrators can change the settings.")).toBeInTheDocument();
});
