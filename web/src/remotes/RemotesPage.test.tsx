import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { RemotesPage } from "@/remotes/RemotesPage";
import { remote } from "@/test/fixtures";
import { ADMIN, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

const TOKEN = "opl_0192f0c15d2e70008000000000000000_secret";
const NEW_FP = `sha256:${"cd".repeat(32)}`;

function mockRemotes(...items: ReturnType<typeof remote>[]) {
  server.use(http.get(apiUrl("/remotes"), () => HttpResponse.json({ revision: 1, items })));
}

it("shows a card for each remote with its status", async () => {
  mockRemotes(
    remote("dr"),
    remote("lab", {
      reachable: false,
      error: "connection refused",
      cluster_name: null,
      version: null,
      nodes: null,
      vms: null,
      cpus: null,
      memory_gib: null,
    }),
  );
  renderPage(<RemotesPage />);
  const dr = await screen.findByRole("group", { name: /^dr/ });
  expect(within(dr).getByRole("link", { name: "dr" })).toHaveAttribute("href", "/remotes/dr");
  expect(dr).toHaveTextContent("Reachable");
  expect(dr).toHaveTextContent("https://dr.example:8006");
  expect(dr).toHaveTextContent("Version0.37.0");
  expect(dr).toHaveTextContent("Nodes1 / 1 online");
  expect(dr).toHaveTextContent("VMs1 / 2 running");
  expect(dr).toHaveTextContent("CPUs8");
  expect(dr).toHaveTextContent("Memory32 GiB");
  const lab = screen.getByRole("group", { name: /^lab/ });
  expect(lab).toHaveTextContent("Unreachable");
  expect(lab).toHaveTextContent("connection refused");
  expect(lab).toHaveTextContent("Version—");
  expect(within(lab).getByRole("button", { name: "Refresh lab" })).toBeInTheDocument();
});

it("shows the list to viewers without changes", async () => {
  mockRemotes(remote("dr"));
  renderPage(<RemotesPage />, { as: { ...ADMIN, role: "viewer" } });
  expect(await screen.findByRole("link", { name: "dr" })).toBeInTheDocument();
  for (const name of ["Add", "Refresh dr", "Edit dr", "Delete dr"]) {
    expect(screen.queryByRole("button", { name })).not.toBeInTheDocument();
  }
});

it("adds a remote after the CA fingerprint is fetched and confirmed", async () => {
  mockRemotes();
  let probed: unknown;
  let body: unknown;
  server.use(
    http.post(apiUrl("/remotes/probe"), async ({ request }) => {
      probed = await request.json();
      return HttpResponse.json({ url: "https://dr.example:8006", ca_fingerprint: NEW_FP });
    }),
    http.post(apiUrl("/remotes"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(remote("dr"), { status: 201 });
    }),
  );
  renderPage(<RemotesPage />);
  expect(await screen.findByText("No remote clusters.")).toBeInTheDocument();
  await userEvent.click(screen.getByRole("button", { name: "Add" }));
  const dialog = await screen.findByRole("dialog", { name: "Add a remote cluster" });
  await userEvent.type(within(dialog).getByLabelText("Name"), "dr");
  await userEvent.type(within(dialog).getByLabelText("URL"), "https://dr.example:8006/");
  await userEvent.click(within(dialog).getByRole("button", { name: "Add" }));
  expect(within(dialog).getByText("Get the CA of this URL first.")).toBeVisible();
  expect(within(dialog).getByText("Enter an API token of the remote cluster.")).toBeVisible();

  await userEvent.click(within(dialog).getByRole("button", { name: "Get the CA" }));
  expect(await within(dialog).findByText(NEW_FP)).toBeVisible();
  // 지문은 이름이 붙은 값(dt·dd)이다.
  expect(within(dialog).getByRole("term")).toHaveTextContent("CA fingerprint");
  expect(within(dialog).getByRole("definition")).toHaveTextContent(NEW_FP);
  expect(probed).toEqual({ url: "https://dr.example:8006/" });
  // 서버가 정규화한 URL로 바꾼다.
  expect(within(dialog).getByLabelText("URL")).toHaveValue("https://dr.example:8006");
  await userEvent.type(within(dialog).getByLabelText("API token"), TOKEN);
  await userEvent.click(within(dialog).getByRole("button", { name: "Add" }));
  expect(
    within(dialog).getByText("Check the fingerprint against the remote cluster and confirm it."),
  ).toBeVisible();
  expect(body).toBeUndefined();

  await userEvent.click(
    within(dialog).getByRole("checkbox", { name: /matches the CA fingerprint/ }),
  );
  await userEvent.click(within(dialog).getByRole("button", { name: "Add" }));
  expect(await screen.findByText("Added remote cluster dr.")).toBeInTheDocument();
  expect(body).toEqual({
    name: "dr",
    url: "https://dr.example:8006",
    ca_fingerprint: NEW_FP,
    token: TOKEN,
  });
});

it("asks for the CA again when the URL changes and shows why it could not be fetched", async () => {
  mockRemotes();
  server.use(
    http.post(apiUrl("/remotes/probe"), () =>
      HttpResponse.json(
        { code: "remote_unreachable", message: "connection refused" },
        { status: 502 },
      ),
    ),
  );
  renderPage(<RemotesPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Add" }));
  const dialog = await screen.findByRole("dialog");
  await userEvent.type(within(dialog).getByLabelText("URL"), "https://gone.example");
  await userEvent.click(within(dialog).getByRole("button", { name: "Get the CA" }));
  expect(
    await within(dialog).findByText(
      "Could not get the CA: Cannot reach the remote cluster (connection refused).",
    ),
  ).toBeVisible();
});

it("shows why the server refused the remote", async () => {
  mockRemotes();
  server.use(
    http.post(apiUrl("/remotes/probe"), () =>
      HttpResponse.json({ url: "https://dr.example", ca_fingerprint: NEW_FP }),
    ),
    http.post(apiUrl("/remotes"), () =>
      HttpResponse.json(
        { code: "remote_unauthorized", message: "the remote cluster rejected the token" },
        { status: 400 },
      ),
    ),
  );
  renderPage(<RemotesPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Add" }));
  const dialog = await screen.findByRole("dialog");
  await userEvent.type(within(dialog).getByLabelText("Name"), "dr");
  await userEvent.type(within(dialog).getByLabelText("URL"), "https://dr.example");
  await userEvent.click(within(dialog).getByRole("button", { name: "Get the CA" }));
  await userEvent.click(await within(dialog).findByRole("checkbox"));
  await userEvent.type(within(dialog).getByLabelText("API token"), TOKEN);
  await userEvent.click(within(dialog).getByRole("button", { name: "Add" }));
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "The remote cluster rejected the API token.",
  );
});

it("edits a remote, keeping the token unless the URL or CA changes", async () => {
  const dr = remote("dr");
  mockRemotes(dr);
  const bodies: unknown[] = [];
  server.use(
    http.put(apiUrl("/remotes/dr"), async ({ request }) => {
      bodies.push(await request.json());
      expect(request.headers.get("If-Match")).toBeNull();
      return HttpResponse.json(dr);
    }),
    http.post(apiUrl("/remotes/probe"), () =>
      HttpResponse.json({ url: "https://dr2.example", ca_fingerprint: dr.ca_fingerprint }),
    ),
  );
  renderPage(<RemotesPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Edit dr" }));
  let dialog = await screen.findByRole("dialog", { name: "Edit remote cluster dr" });
  expect(within(dialog).queryByLabelText("Name")).not.toBeInTheDocument();
  expect(within(dialog).getByLabelText("URL")).toHaveValue(dr.url);
  expect(within(dialog).getByText(dr.ca_fingerprint)).toBeVisible();
  // 저장된 지문은 이미 확인했다.
  expect(within(dialog).queryByRole("checkbox")).not.toBeInTheDocument();
  expect(within(dialog).getByText("A token is saved. Leave this empty to keep it.")).toBeVisible();
  await userEvent.click(within(dialog).getByRole("button", { name: "Save" }));
  expect(await screen.findByText("Saved remote cluster dr.")).toBeInTheDocument();
  expect(bodies).toEqual([{ url: dr.url, ca_fingerprint: dr.ca_fingerprint }]);

  await userEvent.click(screen.getByRole("button", { name: "Edit dr" }));
  dialog = await screen.findByRole("dialog", { name: "Edit remote cluster dr" });
  await userEvent.clear(within(dialog).getByLabelText("URL"));
  await userEvent.type(within(dialog).getByLabelText("URL"), "https://dr2.example");
  expect(within(dialog).queryByText(dr.ca_fingerprint)).not.toBeInTheDocument();
  await userEvent.click(within(dialog).getByRole("button", { name: "Get the CA" }));
  await userEvent.click(await within(dialog).findByRole("checkbox"));
  await userEvent.click(within(dialog).getByRole("button", { name: "Save" }));
  expect(
    within(dialog).getByText("Enter the API token again: the URL or CA changed."),
  ).toBeVisible();
  await userEvent.type(within(dialog).getByLabelText("API token"), TOKEN);
  await userEvent.click(within(dialog).getByRole("button", { name: "Save" }));
  await vi.waitFor(() => expect(bodies).toHaveLength(2));
  expect(bodies[1]).toEqual({
    url: "https://dr2.example",
    ca_fingerprint: dr.ca_fingerprint,
    token: TOKEN,
  });
});

it("refreshes and deletes a remote", async () => {
  let refreshed = false;
  let deleted = false;
  server.use(
    http.get(apiUrl("/remotes"), () =>
      HttpResponse.json({
        revision: 1,
        items: deleted ? [] : [refreshed ? remote("dr", { version: "0.38.0" }) : remote("dr")],
      }),
    ),
    http.post(apiUrl("/remotes/dr/actions/refresh"), () => {
      refreshed = true;
      return HttpResponse.json(remote("dr", { version: "0.38.0" }));
    }),
    http.delete(apiUrl("/remotes/dr"), () => {
      deleted = true;
      return new HttpResponse(null, { status: 204 });
    }),
  );
  renderPage(<RemotesPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Refresh dr" }));
  expect(await screen.findByText("0.38.0")).toBeInTheDocument();

  await userEvent.click(screen.getByRole("button", { name: "Delete dr" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Delete remote cluster dr?" });
  await userEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
  expect(await screen.findByText("Deleted remote cluster dr.")).toBeInTheDocument();
  expect(await screen.findByText("No remote clusters.")).toBeInTheDocument();
});

it("shows why a refresh failed", async () => {
  mockRemotes(remote("dr"));
  server.use(
    http.post(apiUrl("/remotes/dr/actions/refresh"), () =>
      HttpResponse.json({ code: "not_leader", message: "x" }, { status: 503 }),
    ),
  );
  renderPage(<RemotesPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Refresh dr" }));
  expect(
    await screen.findByText(
      "Could not refresh dr: The cluster leader is changing. Try again shortly.",
    ),
  ).toBeInTheDocument();
});
