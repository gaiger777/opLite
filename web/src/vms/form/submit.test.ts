import { http, HttpResponse } from "msw";

import { vm } from "@/test/fixtures";
import { apiUrl, server } from "@/test/server";
import { fromVm } from "@/vms/form";
import { isConflict, updateVm } from "@/vms/form/submit";

it("sends the read revision as If-Match and reports a conflict", async () => {
  const web = vm("web", "stopped", { labels: { team: "a" } });
  const sent: { ifMatch: string | null; body: unknown }[] = [];
  server.use(
    http.put(apiUrl(`/vms/${web.id}`), async ({ request }) => {
      sent.push({ ifMatch: request.headers.get("If-Match"), body: await request.json() });
      return HttpResponse.json(
        { code: "revision_mismatch", message: "expected 1, found 2" },
        { status: 412 },
      );
    }),
  );
  const err: unknown = await updateVm(web, 1, fromVm(web)).catch((e: unknown) => e);
  expect(isConflict(err)).toBe(true);
  expect(isConflict(new Error("boom"))).toBe(false);
  expect(sent).toEqual([
    {
      ifMatch: "1",
      body: expect.objectContaining({ labels: { team: "a" }, spec: expect.anything() }),
    },
  ]);
});
