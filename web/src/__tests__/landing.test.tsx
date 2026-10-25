import { act, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { Landing, Repository } from "../api/types";
import { dictionaries } from "../i18n";
import { applyChange } from "../query/events";
import { apiError, fakeApi, json, renderApp, SESSION } from "../test/fakeApi";
import { BASE, HEAD, landing, pull } from "./fixtures";

const P = "/api/v1/repos/acme/demo/pulls/3";
const REPO: Repository = {
  path: "acme/demo", default_branch: "main", clone_url: "", url: "", protected_branches: ["main"],
  required_checks: ["test"], required_approvals: 1, auto_land: false, ci_network: false, labels: [], revision: 1,
  upstream: { provider: "github", host: "GitHub", repository: "example/demo", url: "https://github.com/example/demo", reader: { member: "agent", chosen: false }, following: { tip: BASE, checked_at: null, followed_at: null, error: null, stopped: null } },
};
const PREVIEW = { host: "GitHub", repository: "example/demo", head: "topic", base: "main", title: "Change", body: "", account: "agent", login: "agent-bot" };
function routes(l: Landing | null, extra: Parameters<typeof fakeApi>[0] = {}) {
  const p = pull({ landing: l, author: "agent", approvals: 1 });
  return {
    "GET /api/v1/session": json(200, SESSION),
    "GET /api/v1/workspaces": json(200, { items: [{ name: "acme", lanes: [] }], next_cursor: null }),
    "GET /api/v1/admin/diagnostics": json(200, {}),
    "GET /api/v1/members": json(200, { items: [], next_cursor: null }),
    "GET /api/v1/repos": json(200, { items: [REPO], next_cursor: null }),
    "GET /api/v1/repos/acme/demo": json(200, REPO),
    "GET /api/v1/repos/acme/demo/pulls": json(200, { items: [p], next_cursor: null }),
    "GET /api/v1/repos/acme/demo/landing": json(200, { items: l ? [l] : [] }),
    "GET /api/v1/repos/acme/demo/branches/main": json(200, { name: "main", sha: BASE }),
    [`GET /api/v1/repos/acme/demo/commits/${BASE}/checks`]: json(200, { sha: BASE, checks: [], checks_expected: false }),
    [`GET ${P}`]: json(200, p),
    [`GET ${P}/checks`]: json(200, { head_sha: HEAD, base_sha: BASE, behind_by: 2, checks: [{ name: "test", status: "completed", conclusion: "success" }], checks_expected: true, base_checks: 1 }),
    [`GET ${P}/activity`]: json(200, { comments: [], reviews: [], threads: [] }),
    [`GET ${P}/threads`]: json(200, { items: [], next_cursor: null }),
    [`GET ${P}/publish`]: json(200, PREVIEW),
    ...extra,
  };
}

describe.each(["en", "ja"] as const)("landing queue (%s)", (lang) => {
  const words = dictionaries[lang];
  it("shows the head, approval and gate stages, with the queue position and leader's retry reason", async () => {
    const first = { number: 2, published: true, state: "passed", state_at: "2026-10-25T10:00:00Z", error: "Host unavailable" } as const;
    fakeApi(routes(landing({ state: "waiting", position: 2, gate: null, first })));
    renderApp("/acme/demo/-/pulls/3", lang);
    const card = await screen.findByRole("region", { name: words["publish.title"].replace("{host}", "GitHub") });
    const stages = within(card).getAllByRole("region");
    expect(stages.map((s) => s.getAttribute("aria-label"))).toEqual([words["landing.headChecks"], words["landing.approvals"], words["landing.gate"]]);
    expect(within(card).getByText(words["landing.position"].replace("{position}", "2"))).toBeInTheDocument();
    expect(within(card).getByRole("link", { name: "!2" })).toHaveAttribute("href", "/acme/demo/-/pulls/2");
    expect(within(card).getByText(new RegExp(words["landing.retrying"].replace(/[()]/g, "\\$&")))).toBeInTheDocument();
    expect(within(card).getByText("Host unavailable")).toBeInTheDocument();
    expect(within(card).getByRole("button", { name: words["publish.submit"].replace("{host}", "GitHub") })).toBeDisabled();
    expect(card.querySelector('time[datetime="2026-10-25T10:00:00Z"]')).toBeInTheDocument();
  });

  it.each(["gating", "left"] as const)("links gate jobs and explains recovery (%s)", async (state) => {
    fakeApi(routes(landing({ state, reason: state === "left" ? { code: "gate_failed", message: "Gate CI failed", jobs: ["test"], files: [] } : null }), {
      [`GET ${P}/checks`]: json(200, { head_sha: HEAD, base_sha: BASE, behind_by: 2, checks: [{ name: "gate/test", status: "completed", conclusion: "failure", attempt_id: "gate_attempt", attempt: 1 }], checks_expected: true, base_checks: 1 }),
      "GET /api/v1/repos/acme/demo/jobs/gate_attempt": json(200, { id: "gate_attempt", job: "test", sha: HEAD, status: "completed", conclusion: "failure", attempt: 1, artifact_names: [] }),
    }));
    renderApp("/acme/demo/-/pulls/3", lang);
    const gate = await screen.findByRole("region", { name: words["landing.gate"] });
    expect(await within(gate).findByRole("link", { name: "gate/test" })).toHaveAttribute("href", "/acme/demo/-/jobs/gate_attempt");
    if (state === "left") {
      expect(within(gate).getByText(words["landing.retryGate"])).toBeInTheDocument();
      expect(within(gate).getByText(words["landing.failedJobs"].replace("{jobs}", "test"))).toBeInTheDocument();
    }
  });

  it("shows conflicts, host failures and stopped-publication reasons with their recovery", async () => {
    const items = [
      landing({ number: 3, state: "left", position: null, reason: { code: "conflict", message: "Merge conflict", jobs: [], files: ["src/main.rs"] } }),
      landing({ number: 4, state: "left", position: null, reason: { code: "host_failed", message: "Host check failed", jobs: ["github/build"], files: [] } }),
      landing({ number: 5, state: "left", position: null, reason: { code: "publish_stopped", message: "Branch changed on host", jobs: [], files: [] } }),
    ];
    fakeApi(routes(null, { "GET /api/v1/repos/acme/demo/landing": json(200, { items }) }));
    renderApp("/acme/demo", lang);
    const queue = await screen.findByRole("region", { name: words["landing.queue"] });
    expect(await within(queue).findByText(words["landing.conflicts"].replace("{files}", "src/main.rs"))).toBeInTheDocument();
    expect(within(queue).getByText(words["landing.retryHost"])).toBeInTheDocument();
    expect(within(queue).getByText(words["landing.publishAgain"])).toBeInTheDocument();
    expect(within(queue).getByText("Branch changed on host")).toBeInTheDocument();
  });

  it("orders each base queue, separates departures and shows the same position on the pull list", async () => {
    const first = { number: 2, published: true, state: "passed", state_at: "2026-10-25T10:00:00Z", error: null } as const;
    const waiting = landing({ state: "waiting", position: 2, first });
    fakeApi(routes(waiting, { "GET /api/v1/repos/acme/demo/landing": json(200, { items: [waiting, landing({ number: 2, first }), landing({ number: 4, state: "left", position: null, reason: { code: "draft", message: "Draft", jobs: [], files: [] } })] }) }));
    renderApp("/acme/demo", lang);
    const queue = await screen.findByRole("region", { name: words["landing.queue"] });
    await within(queue).findByRole("region", { name: "main" });
    // An ordered list for each base and a separate departure list.
    const main = within(queue).getByRole("region", { name: "main" });
    expect(within(main).getAllByRole("listitem").map((l) => within(l).getAllByRole("link")[0]?.textContent)).toEqual(["!2", "!3"]);
    expect(within(queue).getByText(words["landing.hostWaiting"], { exact: true })).toBeInTheDocument();
    const departed = within(queue).getByRole("region", { name: words["landing.left"] });
    expect(within(departed).getByText("Draft")).toBeInTheDocument();
    const pulls = await screen.findByRole("table", { name: words["pulls.title"] });
    expect(within(pulls).getByText(words["landing.position"].replace("{position}", "2"))).toBeInTheDocument();
  });

  it("uses Branvil's icon for its own comments", async () => {
    fakeApi(routes(null, { [`GET ${P}/activity`]: json(200, { comments: [{ id: "gate_failure", author: "branvil", body: "Gate failed", created_at: "2026-10-25T10:00:00Z", reactions: [], meta: null, url: "" }], reviews: [], threads: [] }) }));
    renderApp("/acme/demo/-/pulls/3", lang);
    expect(await screen.findByRole("img", { name: "Branvil" })).toBeInTheDocument();
  });
});

it("counts elapsed time and refreshes a waiting pull when its leader changes", async () => {
  vi.useFakeTimers({ toFake: ["Date", "setInterval", "clearInterval"] });
  vi.setSystemTime(new Date("2026-10-25T10:01:00Z"));
  const waiting = landing({ state: "waiting", position: 2, first: { number: 2, published: true, state: "gating", state_at: "2026-10-25T10:00:00Z", error: null } });
  const refreshed = { ...waiting, first: { ...waiting.first!, state: "passed" as const } };
  fakeApi(routes(waiting, { [`GET ${P}`]: [json(200, pull({ landing: waiting })), json(200, pull({ landing: refreshed }))] }));
  const { client } = renderApp("/acme/demo/-/pulls/3");
  const gate = await screen.findByRole("region", { name: dictionaries.en["landing.gate"] });
  expect(within(gate).getByText("1m 0s")).toBeInTheDocument();
  act(() => vi.advanceTimersByTime(2000));
  expect(within(gate).getByText("1m 2s")).toBeInTheDocument();
  act(() => applyChange({ kind: "pull", path: "acme/demo", id: "2" }, client));
  await waitFor(() => expect(within(gate).getByText(/Waiting for GitHub merge/)).toBeInTheDocument());
});

for (const host of [false, true]) {
  it.each([
    { why: "not queued", l: null },
    { why: "waiting", l: landing({ state: "waiting", position: 2 }) },
    { why: "gating", l: landing({ state: "gating" }) },
    { why: "left", l: landing({ state: "left", position: null }) },
    { why: "outdated head", l: landing({ head_sha: BASE }) },
    { why: "outdated base", l: landing({ base_sha: HEAD }) },
    { why: "not the first", l: landing({ position: 2 }) },
  ])(`blocks ${host ? "publishing" : "merging"} with successful head checks and approvals but $why`, async ({ l }) => {
    fakeApi(routes(l, { "GET /api/v1/repos/acme/demo": json(200, { ...REPO, upstream: host ? REPO.upstream : null }) }));
    renderApp("/acme/demo/-/pulls/3");
    const action = host ? "Publish to GitHub" : dictionaries.en["merge.submit"];
    await screen.findByRole("region", { name: dictionaries.en["landing.gate"] });
    await waitFor(() => expect(screen.queryByText(/Required checks not yet successful/)).not.toBeInTheDocument());
    expect(screen.getByRole("button", { name: action })).toBeDisabled();
  });
}


describe.each(["en", "ja"] as const)("resuming stopped publication (%s)", (lang) => {
  const words = dictionaries[lang];
  const stopped = {
    provider: "github", repository: "example/demo", account: "agent", number: 42,
    url: "https://github.com/example/demo/pull/42", state: "open" as const, pushed: HEAD,
    published_at: "2026-10-25T10:00:00Z", stopped: "Host branch changed", error: null, auto: true,
  };
  const left = landing({ state: "left", position: null, reason: { code: "publish_stopped", message: "Host branch changed", jobs: [], files: [] } });

  it("rejoins behind another pull, waits for its gate, then confirms replacing the host branch", async () => {
    let current = pull({ upstream: stopped, landing: left, approvals: 1 });
    const waiting = { ...current, landing: landing({ resume_publish: true, state: "waiting", position: 2, first: { number: 2, published: true, state: "gating", state_at: "2026-10-25T10:00:00Z", error: null } }) };
    const fake = fakeApi(routes(left, {
      [`GET ${P}`]: () => json(200, current),
      [`POST ${P}/landing/resume`]: () => { current = waiting; return json(200, current); },
      [`POST ${P}/publish`]: () => { current = { ...current, upstream: { ...stopped, stopped: null } }; return json(200, current); },
    }));
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(false);
    const user = userEvent.setup();
    const { client } = renderApp("/acme/demo/-/pulls/3", lang);
    const resume = await screen.findByRole("button", { name: words["landing.resume"] });
    await waitFor(() => expect(resume).toBeEnabled());
    await user.click(resume);
    expect(fake.calls("POST", `${P}/landing/resume`)[0]?.body).toEqual({ expected_head: HEAD, expected_base: BASE });
    const again = await screen.findByRole("button", { name: words["publish.again"].replace("{host}", "GitHub") });
    expect(again).toBeDisabled();
    expect(screen.queryByRole("button", { name: words["landing.resume"] })).not.toBeInTheDocument();
    expect(screen.getByText(words["landing.position"].replace("{position}", "2"))).toBeInTheDocument();
    expect(confirm).not.toHaveBeenCalled();
    expect(fake.calls("POST", `${P}/publish`)).toHaveLength(0);
    current = { ...current, landing: landing({ resume_publish: true, state: "gating" }) };
    act(() => applyChange({ kind: "pull", path: "acme/demo", id: "3" }, client));
    const gate = screen.getByRole("region", { name: words["landing.gate"] });
    expect(await within(gate).findByText(words["landing.gating"])).toBeInTheDocument();
    expect(again).toBeDisabled();
    current = { ...current, landing: landing({ resume_publish: true, published: false, first: { ...landing().first!, published: false } }) };
    act(() => applyChange({ kind: "pull", path: "acme/demo", id: "3" }, client));
    await waitFor(() => expect(again).toBeEnabled());
    expect(within(gate).getByText(words["landing.publishWaiting"])).toBeInTheDocument();
    await user.click(again);
    expect(confirm).toHaveBeenCalledWith(words["publish.confirmAgain"].replace("{host}", "GitHub"));
    expect(fake.calls("POST", `${P}/publish`)).toHaveLength(0);
    confirm.mockReturnValue(true);
    await user.click(again);
    await waitFor(() => expect(fake.calls("POST", `${P}/publish`)).toHaveLength(1));
    expect(fake.calls("POST", `${P}/publish`)[0]?.body).toEqual({ expected_head: HEAD, expected_base: BASE });
  });

  it.each(["checks", "approvals", "draft", "mismatched head"] as const)("cannot resume without current readiness (%s)", async (blockedBy) => {
    fakeApi(routes(left, {
      [`GET ${P}`]: json(200, pull({ upstream: stopped, landing: null, approvals: blockedBy === "approvals" ? 0 : 1, draft: blockedBy === "draft" })),
      [`GET ${P}/checks`]: json(200, { head_sha: blockedBy === "mismatched head" ? BASE : HEAD, base_sha: BASE, checks: [{ name: "test", status: "completed", conclusion: blockedBy === "checks" ? "failure" : "success" }], checks_expected: true, base_checks: 1 }),
    }));
    renderApp("/acme/demo/-/pulls/3", lang);
    const resume = await screen.findByRole("button", { name: words["landing.resume"] });
    await waitFor(() => expect(screen.queryByText(words["common.loading"])).not.toBeInTheDocument());
    expect(resume).toBeDisabled();
  });

  it("shows a resume refusal without publishing or dropping the stopped state", async () => {
    const fake = fakeApi(routes(left, {
      [`GET ${P}`]: json(200, pull({ upstream: stopped, landing: left, approvals: 1 })),
      [`POST ${P}/landing/resume`]: apiError(409, "head_moved", "Head changed"),
    }));
    const user = userEvent.setup();
    renderApp("/acme/demo/-/pulls/3", lang);
    const resume = await screen.findByRole("button", { name: words["landing.resume"] });
    await waitFor(() => expect(resume).toBeEnabled());
    await user.click(resume);
    expect(await screen.findByText(words["merge.headMoved"])).toBeInTheDocument();
    expect(fake.calls("POST", `${P}/publish`)).toHaveLength(0);
    expect(screen.getByRole("button", { name: words["landing.resume"] })).toBeInTheDocument();
  });
});

describe.each(["en", "ja"] as const)("publication readiness (%s)", (lang) => {
  const words = dictionaries[lang];
  it.each([
    { published: false, error: null, label: "landing.publishWaiting" },
    { published: true, error: null, label: "landing.hostWaiting" },
    { published: false, error: "Host unavailable", label: "landing.retrying" },
  ] as const)("shows the leader's actual publication state ($published, $error)", async ({ published, error, label }) => {
    const leader = landing({ published, error, first: { ...landing().first!, published, error } });
    const waiting = landing({ number: 4, state: "waiting", position: 2, first: leader.first });
    fakeApi(routes(leader, { "GET /api/v1/repos/acme/demo/landing": json(200, { items: [leader, waiting] }) }));
    renderApp("/acme/demo", lang);
    const queue = await screen.findByRole("region", { name: words["landing.queue"] });
    expect(await within(queue).findByText(words[label], { exact: true })).toBeInTheDocument();
    const first = within(queue).getAllByRole("listitem")[0]!;
    expect(within(first).getByText((_, element) => element?.tagName === "P" && !!element.textContent?.includes(words["landing.first"]) && element.textContent.includes(words[label]))).toBeInTheDocument();
    if (error) expect(within(queue).getByText(error)).toBeInTheDocument();
  });
});


it.each(["en", "ja"] as const)("starts the publication wait clock again when publication or its error changes (%s)", async (lang) => {
  vi.useFakeTimers({ toFake: ["Date", "setInterval", "clearInterval"] });
  vi.setSystemTime(new Date("2026-10-25T10:02:00Z"));
  const words = dictionaries[lang];
  const entry = (published: boolean, error: string | null, at: string) => landing({
    published, error, state_at: at, first: { ...landing().first!, published, error, state_at: at },
  });
  let current = pull({ approvals: 1, landing: entry(false, null, "2026-10-25T10:00:00Z") });
  fakeApi(routes(current.landing, { [`GET ${P}`]: () => json(200, current) }));
  const { client } = renderApp("/acme/demo/-/pulls/3", lang);
  const gate = await screen.findByRole("region", { name: words["landing.gate"] });
  expect(within(gate).getByText(words["landing.publishWaiting"])).toBeInTheDocument();
  expect(within(gate).getByText(lang === "en" ? "2m 0s" : "2分0秒")).toBeInTheDocument();
  expect(gate.querySelector('time[datetime="2026-10-25T10:00:00Z"]')).toBeInTheDocument();

  current = { ...current, landing: entry(true, null, "2026-10-25T10:01:00Z") };
  act(() => applyChange({ kind: "pull", path: "acme/demo", id: "3" }, client));
  expect(await within(gate).findByText(words["landing.hostWaiting"])).toBeInTheDocument();
  expect(within(gate).getByText(lang === "en" ? "1m 0s" : "1分0秒")).toBeInTheDocument();
  expect(gate.querySelector('time[datetime="2026-10-25T10:01:00Z"]')).toBeInTheDocument();

  current = { ...current, landing: entry(true, "Host unavailable", "2026-10-25T10:01:30Z") };
  act(() => applyChange({ kind: "pull", path: "acme/demo", id: "3" }, client));
  expect(await within(gate).findByText(words["landing.retrying"])).toBeInTheDocument();
  expect(within(gate).getByText("Host unavailable")).toBeInTheDocument();
  expect(within(gate).getByText(lang === "en" ? "30s" : "30秒")).toBeInTheDocument();
  expect(gate.querySelector('time[datetime="2026-10-25T10:01:30Z"]')).toBeInTheDocument();
});
