import { MantineProvider } from "@mantine/core";
import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { Landing, LandingNext, Repository } from "../api/types";
import { dictionaries, I18nProvider } from "../i18n";
import { applyChange } from "../query/events";
import { apiError, fakeApi, json, renderApp, SESSION } from "../test/fakeApi";
import { BASE, HEAD, landing, pull } from "./fixtures";
import { theme } from "../theme";
import { LandingBadge, LandingReason } from "../views/pull/LandingStatus";

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

const STOP: Record<LandingNext, string> = {
  gate: "gate", publishing: "host", owner_publish: "host", host_checks: "host", host_closed: "host", owner_fix_auto: "host",
  owner_merge_host: "merge", auto_merge: "merge", owner_merge: "merge", following: "base", owner_fix_following: "base",
};
const stops = (journey: HTMLElement, state?: string) => [...journey.querySelectorAll<HTMLElement>(state ? `[data-stop][data-state="${state}"]` : "[data-stop]")].map((e) => e.dataset.stop);
const badges = (journey: HTMLElement) => [...journey.querySelectorAll(".mantine-Badge-root")].map((e) => e.textContent);
const checksLoaded = async () => within(await screen.findByRole("region", { name: "Checks" })).findByTitle(HEAD);
const NEXT: LandingNext[] = ["gate", "publishing", "owner_publish", "host_checks", "owner_merge_host", "following", "owner_fix_following", "host_closed", "owner_fix_auto", "auto_merge", "owner_merge"];
const entry = (next: LandingNext, extra: Partial<Landing> = {}) => landing({ first: { ...landing().first!, next, next_detail: "Diagnostic detail" }, ...extra });

describe.each(["en", "ja"] as const)("queue path (%s)", (lang) => {
  const words = dictionaries[lang];
  const journeyName = () => words["landing.journey"].replace("{base}", "main");
  const label = (next: LandingNext, base = "main") => words[`landing.next.${next}`].replaceAll("{host}", "GitHub").replaceAll("{base}", base);
  it.each(NEXT)("places the leader for %s and links only human actions", async (next) => {
    fakeApi(routes(entry(next)));
    renderApp("/acme/demo", lang);
    const queue = await screen.findByRole("region", { name: words["landing.queue"] });
    expect(await within(queue).findByText(label(next))).toBeInTheDocument();
    expect(within(queue).getByText("Diagnostic detail")).toBeInTheDocument();
    const slot = next === "gate" ? "gate" : ["publishing", "owner_publish"].includes(next) ? "transit" : "host";
    expect(queue.querySelector(`[data-stage=${slot}] [data-pr="3"]`)).toBeInTheDocument();
    expect(within(queue).getByText("Fix login")).toBeInTheDocument();
    const list = await screen.findByRole("table", { name: words["pulls.title"] });
    expect(await within(list).findByText(words[`landing.badge.${next}`].replaceAll("{host}", "GitHub").replaceAll("{base}", "main"))).toBeInTheDocument();
    const human = next.startsWith("owner_") || next === "host_closed";
    const links = within(queue).queryAllByRole("link");
    expect(links).toHaveLength(human ? 1 : 0);
    if (human) expect(links[0]).toHaveAttribute("href", ["owner_merge_host", "host_closed"].includes(next) ? "https://example.test/pulls/3" : "/acme/demo/-/pulls/3");
  });
  it.each(["auto_merge", "owner_merge"] as const)("draws a local path to the named base (%s)", async (next) => {
    fakeApi(routes(entry(next, { base: "release" }), { "GET /api/v1/repos/acme/demo": json(200, { ...REPO, upstream: null }) }));
    renderApp("/acme/demo", lang);
    const queue = await screen.findByRole("region", { name: words["landing.queue"] });
    expect(await within(queue).findByText(label(next, "release"))).toBeInTheDocument();
    expect(queue.querySelector('[data-stage="host"]')).not.toBeInTheDocument();
    expect(queue.querySelector('[data-stage="transit"] [data-pr="3"]')).toBeInTheDocument();
    expect(queue.querySelector('[data-stage="base"]')).toHaveTextContent("release");
  });
  it.each([true, false])("keeps the base name outside its marker (upstream=%s)", async (host) => {
    const base = "release/next";
    const l = entry(host ? "owner_merge_host" : "owner_merge", {base});
    fakeApi(routes(l, {"GET /api/v1/repos/acme/demo": json(200, {...REPO, upstream:host ? REPO.upstream : null})}));
    renderApp("/acme/demo", lang);
    const region = await screen.findByRole("region", {name:words["landing.queue"]});
    await waitFor(() => expect(region.querySelector('[data-stage="base"]')).toBeInTheDocument());
    const stop = region.querySelector('[data-stage="base"]') as HTMLElement;
    expect(stop.querySelector(".landing-marker")).not.toHaveTextContent(base);
    expect(within(stop).getByText(base).closest(".landing-marker")).toBeNull();
  });
  it("draws nearest three waiters in reverse order and groups the remainder", async () => {
    const leader = entry("host_checks");
    const items = [leader, ...[4,5,6,7,8].map((n) => landing({ number: n, title: `Waiting ${n}`, state: "waiting", position: n-2, first: leader.first }))];
    fakeApi(routes(leader, { "GET /api/v1/repos/acme/demo/landing": json(200, { items: items.reverse() }) }));
    renderApp("/acme/demo", lang);
    const queue = await screen.findByRole("region", { name: words["landing.queue"] });
    expect(await within(queue).findByText(words["landing.others"].replace("{count}", "2"))).toBeInTheDocument();
    expect([...queue.querySelectorAll('[data-stage="waiting"] [data-pr]')].map((e) => e.getAttribute("data-pr"))).toEqual(["6", "5", "4"]);
    expect(within(queue).getByText(words["landing.waitFor"].replace("{first}", "3"))).toBeInTheDocument();
  });
  it("counts only completed required gate jobs", async () => {
    fakeApi(routes(entry("gate", { state: "gating" }), {
      "GET /api/v1/repos/acme/demo": json(200, { ...REPO, required_checks: ["test", "lint", "build"] }),
      [`GET ${P}/checks`]: json(200, { head_sha: HEAD, checks: [{ name: "gate/test", status: "completed", conclusion: "failure" }, { name: "gate/lint", status: "in_progress" }, { name: "gate/optional", status: "completed", conclusion: "success" }] }),
    }));
    renderApp("/acme/demo", lang);
    const queue = await screen.findByRole("region", { name: words["landing.queue"] });
    expect(await within(queue).findByText("1/3")).toBeInTheDocument();
  });
  it("filters departures and sorts recent merges by state_at rather than sequence", async () => {
    const left = (n: number, code: string, extra: Partial<Landing> = {}) => landing({ number: n, title: `Title ${n}`, state: "left", first: null, position: null, reason: { code, message: "Service English", jobs: [], files: [] }, ...extra });
    const items = [left(1,"conflict"),left(2,"head_moved"),left(3,"draft",{draft:true}),left(4,"conflict",{pull_state:"closed"}), ...[5,6,7,8].map(n=>left(n,"merged",{pull_state:"closed",state_at:`2026-10-${30-n}T00:00:00Z`}))];
    fakeApi(routes(null, { "GET /api/v1/repos/acme/demo/landing": json(200, { items }) }));
    renderApp("/acme/demo", lang);
    const queue = await screen.findByRole("region", { name: words["landing.queue"] });
    const departures = await within(queue).findByRole("region", {name:words["landing.left"]});
    expect(within(departures).getAllByRole("link").map(e=>e.textContent)).toEqual(["!1 Title 1","!2 Title 2"]);
    expect(within(departures).getByText(words["landing.waitRejoin"])).toBeInTheDocument();
    const recent = within(queue).getByRole("region", {name:words["landing.recent"].replace("{base}","main")});
    expect(within(recent).getAllByRole("link").map(e=>e.textContent)).toEqual(["!5 Title 5","!6 Title 6","!7 Title 7"]);
    expect(within(queue).queryByText("Service English")).not.toBeInTheDocument();
  });
  it.each(["conflict","gate_failed","host_failed","publish_stopped","already_in_base","head_moved","base_moved","empty_head","publish_requirements","head_checks","approvals"] as const)("localizes %s", async (code) => {
    fakeApi(routes(landing({state:"left",first:null,reason:{code,message:"Service English",jobs:["build"],files:["src/main.rs"]}})));
    renderApp("/acme/demo",lang);
    const queue = await screen.findByRole("region",{name:words["landing.queue"]});
    expect(await within(queue).findByText(words[`landing.reason.${code}`])).toBeInTheDocument();
    expect(within(queue).queryByText("Service English")!==null).toBe(code==="publish_stopped");
    if(code==="already_in_base") expect(within(queue).getByText(words["landing.closePull"])).toBeInTheDocument();
  });
  it.each(NEXT)("places this pull request at the stop %s names and links only host actions", async (next) => {
    fakeApi(routes(entry(next)));
    renderApp("/acme/demo/-/pulls/3", lang);
    const journey = await screen.findByRole("region", {name:journeyName()});
    expect(await within(journey).findByText(label(next))).toBeInTheDocument();
    expect(within(journey).getByText("Diagnostic detail")).toBeInTheDocument();
    const stop = journey.querySelector('[aria-current="step"]') as HTMLElement;
    expect(stop).toHaveAttribute("data-stop", STOP[next]);
    expect(stop).toHaveAttribute("data-state", next.startsWith("owner_") || next === "host_closed" ? "turn" : "now");
    expect(stops(journey)).toEqual(["conditions", "queue", "gate", "host", "merge", "base"]);
    expect(stops(journey, "done")).toEqual(stops(journey).slice(0, stops(journey).indexOf(STOP[next])));
    const links = within(journey).queryAllByRole("link");
    expect(links.map((a) => a.getAttribute("href"))).toEqual(["owner_merge_host", "host_closed"].includes(next) ? ["https://example.test/pulls/3"] : []);
  });
  it.each(["auto_merge", "owner_merge", "owner_fix_auto"] as const)("merges here without an upstream (%s)", async (next) => {
    fakeApi(routes(entry(next), { "GET /api/v1/repos/acme/demo": json(200, { ...REPO, upstream: null }) }));
    renderApp("/acme/demo/-/pulls/3", lang);
    const journey = await screen.findByRole("region", {name:journeyName()});
    await waitFor(() => expect(journey.querySelector('[aria-current="step"]')).toHaveAttribute("data-stop", "merge"));
    expect(stops(journey)).toEqual(["conditions", "queue", "gate", "merge", "base"]);
  });
  it("names only what the head still needs to join the queue", async () => {
    fakeApi(routes(null, {
      "GET /api/v1/repos/acme/demo":json(200,{...REPO,required_checks:["ok","fail","run","absent"]}),
      [`GET ${P}`]: json(200, pull({ landing: null, approvals: 0, draft: true })),
      [`GET ${P}/checks`]:json(200,{head_sha:HEAD,checks:[{name:"ok",status:"completed",conclusion:"success"},{name:"fail",status:"completed",conclusion:"failure"},{name:"run",status:"queued"}]}),
    }));
    renderApp("/acme/demo/-/pulls/3",lang);
    const journey = await screen.findByRole("region", {name:journeyName()});
    expect(await within(journey).findByText(words["landing.missing"])).toBeInTheDocument();
    await within(journey).findByText(`fail · ${words["landing.check.failure"]}`);
    expect(badges(journey)).toEqual([words["pull.draft"], `fail · ${words["landing.check.failure"]}`, `run · ${words["landing.check.running"]}`, `absent · ${words["landing.check.missing"]}`, words["merge.approvals"].replace("{count}", "0").replace("{required}", "1")]);
    expect(journey.querySelector('[aria-current="step"]')).toHaveAttribute("data-state", "turn");
    expect(within(journey).getByText(words["landing.notQueued"])).toBeInTheDocument();
  });
  it("waits for CI without handing the turn to a person", async () => {
    fakeApi(routes(null, {[`GET ${P}/checks`]:json(200,{head_sha:HEAD,checks:[{name:"test",status:"queued"}]})}));
    renderApp("/acme/demo/-/pulls/3",lang);
    const journey = await screen.findByRole("region", {name:journeyName()});
    expect(await within(journey).findByText(`test · ${words["landing.check.running"]}`)).toBeInTheDocument();
    expect(within(journey).getByText(words["landing.missing"])).toBeInTheDocument();
    expect(journey.querySelector('[aria-current="step"]')).toHaveAttribute("data-state", "now");
  });
  it("says what is in place when nothing visible is missing", async () => {
    fakeApi(routes(null));
    renderApp("/acme/demo/-/pulls/3",lang);
    const journey = await screen.findByRole("region", {name:journeyName()});
    expect(await within(journey).findByText(words["landing.notInQueue"])).toBeInTheDocument();
    expect(journey.querySelector('[aria-current="step"]')).toHaveAttribute("data-state", "now");
    expect(badges(journey)).toEqual([]);
    expect(within(journey).getByText(words["landing.headReady"])).toBeInTheDocument();
  });
  it("waits at its place in the queue", async () => {
    const leader = entry("host_checks", {number:1, first:{...entry("host_checks").first!,number:1}});
    fakeApi(routes(landing({state:"waiting", position:4, first:leader.first})));
    renderApp("/acme/demo/-/pulls/3",lang);
    const journey = await screen.findByRole("region", {name:journeyName()});
    expect(await within(journey).findByText(words["landing.waitingAt"].replace("{position}", "4"))).toBeInTheDocument();
    const stop = journey.querySelector('[aria-current="step"]') as HTMLElement;
    expect(stop).toHaveAttribute("data-stop", "queue");
    expect(stop).toHaveTextContent("4");
    expect(within(journey).getByText(words["landing.waitFor"].replace("{first}", "1"))).toBeInTheDocument();
  });
  it("counts the pull requests waiting behind the leader", async () => {
    const leader = entry("owner_merge_host");
    const items = [leader, ...[4,5].map((n) => landing({ number: n, state: "waiting", position: n-2, first: leader.first })), landing({ number: 6, state: "left", position: null, first: null }), landing({ number: 7, base: "release" })];
    fakeApi(routes(leader, { "GET /api/v1/repos/acme/demo/landing": json(200, { items }) }));
    renderApp("/acme/demo/-/pulls/3",lang);
    const journey = await screen.findByRole("region", {name:journeyName()});
    expect(await within(journey).findByText(words["landing.behind"].replace("{count}", "2"))).toBeInTheDocument();
  });
  it("keeps the recorded clock and its label when only the next action changes", async () => {
    vi.useFakeTimers({toFake:["Date","setInterval","clearInterval"]});
    vi.setSystemTime(new Date("2026-10-25T10:01:00Z"));
    let l=entry("owner_merge_host",{first:{...entry("owner_merge_host").first!,state_at:"2026-10-25T10:00:00Z"}});
    fakeApi(routes(l, {"GET /api/v1/repos/acme/demo/landing": () => json(200, {items: [l]})}));
    const {client} = renderApp("/acme/demo",lang);
    const queue=await screen.findByRole("region",{name:words["landing.queue"]});
    const recorded = lang === "ja" ? "検問・公開状態の記録から" : "Since the recorded gate/publishing update:";
    expect(await within(queue).findByText(recorded, {exact:false})).toBeInTheDocument();
    expect(within(queue).getByText(lang==="ja"?"1分0秒":"1m 0s")).toBeInTheDocument();
    act(()=>vi.advanceTimersByTime(2000));
    for (const next of NEXT) {
      l = {...l, first: {...l.first!, next}};
      act(() => applyChange({kind:"pull", path:"acme/demo", id:"3"}, client));
      await within(queue).findByText(label(next));
      expect(within(queue).getByText(recorded, {exact:false})).toBeInTheDocument();
      expect(within(queue).getByText(lang==="ja"?"1分2秒":"1m 2s")).toBeInTheDocument();
    }
  });
  it.each([
    ["conflict", "warning"], ["gate_failed", "warning"], ["host_failed", "warning"],
    ["publish_stopped", "warning"], ["already_in_base", "warning"], ["empty_head", "warning"],
    ["publish_requirements", "warning"], ["head_checks", "warning"], ["approvals", "warning"],
    ["head_moved", "neutral"], ["base_moved", "neutral"], ["closed", "neutral"],
    ["draft", "neutral"], ["merged", "success"],
  ] as const)("uses the departure reason for both the badge and detail tone (%s)", (code, tone) => {
    const l = landing({state:"left", position:null, first:null, reason:{code,message:"Service English",files:[],jobs:[]}});
    render(<MantineProvider theme={theme} env="test"><I18nProvider initial={lang}><LandingBadge landing={l} /><LandingReason landing={l} /></I18nProvider></MantineProvider>);
    const badge = screen.getByText(words["landing.left"]);
    expect(badge.closest(".mantine-Badge-root")).toHaveStyle(`--badge-color: var(--mantine-color-${tone}-light-color)`);
    const reason = screen.getByText(words[`landing.reason.${code}`]);
    expect(reason).toHaveStyle(tone === "neutral" ? "color: var(--mantine-color-dimmed)" : `color: var(--color-${tone}-text)`);
  });
  it("uses Branvil's icon for service comments",async()=>{
    fakeApi(routes(null,{[`GET ${P}/activity`]:json(200,{comments:[{id:"gate_failure",author:"branvil",body:"Gate failed",created_at:"2026-10-25T10:00:00Z",reactions:[],meta:null,url:""}],reviews:[],threads:[]})}));
    renderApp("/acme/demo/-/pulls/3",lang);
    expect(await screen.findByRole("img",{name:"Branvil"})).toBeInTheDocument();
  });
  it("shows checking for a mismatched head",async()=>{
    fakeApi(routes(null,{[`GET ${P}/checks`]:json(200,{head_sha:BASE,checks:[{name:"test",status:"completed",conclusion:"success"}]})}));
    renderApp("/acme/demo/-/pulls/3",lang);
    const journey=await screen.findByRole("region",{name:journeyName()});
    expect(await within(journey).findByText(`test · ${words["landing.checking"]}`)).toBeInTheDocument();
  });
  it.each(["conflict", "gate_failed", "gating", "passed"] as const)("forgets an old %s gate after the head moves", async (state) => {
    const l = landing({head_sha: BASE, gate: BASE, state: state === "conflict" || state === "gate_failed" ? "left" : state, reason: {code:state, message:"Old failure", jobs:["old-job"], files:["old-file.rs"]}});
    fakeApi(routes(l));
    renderApp("/acme/demo/-/pulls/3", lang);
    const journey = await screen.findByRole("region", {name:journeyName()});
    expect(await within(journey).findByText(words["landing.notInQueue"])).toBeInTheDocument();
    expect(journey.querySelector('[aria-current="step"]')).toHaveAttribute("data-stop", "conditions");
    expect(within(journey).queryByText(/old-job|old-file|Old failure/)).not.toBeInTheDocument();
    expect(within(journey).queryByText(words["landing.retryGate"])).not.toBeInTheDocument();
  });
  it.each([
    ["conflict", "gate"], ["gate_failed", "gate"], ["already_in_base", "gate"], ["host_failed", "host"], ["publish_stopped", "host"],
    ["head_checks", "conditions"], ["approvals", "conditions"], ["empty_head", "conditions"], ["publish_requirements", "conditions"],
  ] as const)("marks where it left the queue (%s)", async (code, stop) => {
    fakeApi(routes(landing({state:"left",first:null,position:null,reason:{code,message:"Service English",jobs:["build"],files:[]}})));
    renderApp("/acme/demo/-/pulls/3", lang);
    const journey = await screen.findByRole("region", {name:journeyName()});
    expect(await within(journey).findByText(words["landing.leftWith"].replace("{reason}", words[`landing.reason.${code}`]))).toBeInTheDocument();
    const current = journey.querySelector('[aria-current="step"]') as HTMLElement;
    expect(current).toHaveAttribute("data-stop", stop);
    expect(current).toHaveAttribute("data-state", "failed");
    expect(within(journey).getByText(words["landing.failedJobs"].replace("{jobs}", "build"))).toBeInTheDocument();
  });
  it("still names what is missing after leaving for it", async () => {
    fakeApi(routes(null, {[`GET ${P}`]: json(200, pull({ approvals: 0, landing: landing({state:"left",first:null,position:null,reason:{code:"approvals",message:"",jobs:[],files:[]}}) }))}));
    renderApp("/acme/demo/-/pulls/3", lang);
    const journey = await screen.findByRole("region", {name:journeyName()});
    expect(await within(journey).findByText(words["merge.approvals"].replace("{count}", "0").replace("{required}", "1"))).toBeInTheDocument();
    expect(journey.querySelector('[aria-current="step"]')).toHaveAttribute("data-state", "failed");
    expect(within(journey).getByText(words["landing.rejoin"])).toBeInTheDocument();
  });
  it("shows the gate's progress on the pull request page", async () => {
    fakeApi(routes(entry("gate", { state: "gating" }), {
      "GET /api/v1/repos/acme/demo": json(200, { ...REPO, required_checks: ["test", "lint", "build"] }),
      [`GET ${P}/checks`]: json(200, { head_sha: HEAD, checks: [{ name: "gate/test", status: "completed", conclusion: "failure" }, { name: "gate/lint", status: "in_progress" }] }),
    }));
    renderApp("/acme/demo/-/pulls/3", lang);
    const journey = await screen.findByRole("region", {name:journeyName()});
    expect(await within(journey).findByText("1/3")).toBeInTheDocument();
  });
  it.each(["draft", "base_moved"] as const)("collects requirements again after leaving for %s", async (code) => {
    fakeApi(routes(landing({state:"left",first:null,position:null,reason:{code,message:"",jobs:[],files:[]}})));
    renderApp("/acme/demo/-/pulls/3", lang);
    const journey = await screen.findByRole("region", {name:journeyName()});
    expect(await within(journey).findByText(words["landing.notInQueue"])).toBeInTheDocument();
    expect(journey.querySelector('[aria-current="step"]')).toHaveAttribute("data-stop", "conditions");
  });
});

for (const host of [false, true]) {
  it.each([true, false])(`accepts only a service path skip for ${host ? "publication" : "merge"} (service=%s)`, async (service) => {
    const path_skip = service ? {paths:["src/**"], bases:[{pull:3,base:"main",base_sha:BASE,merge_base:BASE}]} : null;
    fakeApi(routes(landing({state:"passed",first:{...landing().first!, next:host ? "owner_publish" : "owner_merge"}}), {
      "GET /api/v1/repos/acme/demo": json(200, {...REPO,upstream:host ? REPO.upstream : null}),
      [`GET ${P}/checks`]: json(200,{head_sha:HEAD,base_sha:BASE,checks:[{name:"test",status:"completed",conclusion:"skipped",path_skip}],checks_expected:true}),
    }));
    renderApp("/acme/demo/-/pulls/3");
    const action = await screen.findByRole("button",{name:host ? "Publish to GitHub" : dictionaries.en["merge.submit"]});
    await checksLoaded();
    if (service) expect(action).toBeEnabled(); else expect(action).toBeDisabled();
  });
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
    await checksLoaded();
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
    const waiting = { ...current, landing: landing({ resume_publish: true, state: "waiting", position: 2, first: { ...landing().first!, number: 2, next: "gate", state: "gating" } }) };
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
    expect(screen.getByText(words["landing.waitingAt"].replace("{position}", "2"))).toBeInTheDocument();
    expect(confirm).not.toHaveBeenCalled();
    expect(fake.calls("POST", `${P}/publish`)).toHaveLength(0);
    current = { ...current, landing: landing({ resume_publish: true, state: "gating" }) };
    act(() => applyChange({ kind: "pull", path: "acme/demo", id: "3" }, client));
    const gate = screen.getByRole("region", { name: words["landing.journey"].replace("{base}", "main") });
    expect((await within(gate).findAllByText(words["landing.next.gate"]))[0]).toBeInTheDocument();
    expect(again).toBeDisabled();
    current = { ...current, landing: landing({ resume_publish: true, first: { ...landing().first!, next: "owner_publish" } }) };
    act(() => applyChange({ kind: "pull", path: "acme/demo", id: "3" }, client));
    await waitFor(() => expect(again).toBeEnabled());
    expect(within(gate).getAllByText(words["landing.next.owner_publish"].replace("{host}", "GitHub"))[0]).toBeInTheDocument();
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
