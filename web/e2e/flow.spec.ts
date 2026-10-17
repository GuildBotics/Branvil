// The owner's step of the end-to-end flow (scripts/e2e-flow.sh): a pull
// request whose required check ran for real in a microVM is reviewed (its
// check, the check's log, and the diff) and merged in the GUI, and the ticket
// it closes ends closed in the done lane.
//
// Environment (set by integration/guildbotics/e2e_flow.py):
//   BRANVIL_E2E_BASE_URL   base URL of the running service
//   BRANVIL_E2E_BIN, BRANVIL_E2E_DATA_DIR  how to get an entry link (e2e/enter.ts)
//   BRANVIL_E2E_FLOW_PULL  the pull request's web path, e.g. /acme/demo/-/pulls/1
//   BRANVIL_E2E_FLOW_LOG   a line the check's log must show
//   BRANVIL_E2E_CHANNEL    (optional) `chrome` to use the installed Google Chrome

import { expect, test } from "@playwright/test";
import { enter } from "./enter";

const env = {
  pull: process.env.BRANVIL_E2E_FLOW_PULL as string,
  log: process.env.BRANVIL_E2E_FLOW_LOG as string,
};

test.skip(!env.pull, "BRANVIL_E2E_FLOW_PULL is not set (run scripts/e2e-flow.sh)");
test.use({ locale: "en-US" });

test("the owner reviews and merges after the real CI passed", async ({ page }) => {
  const started = Date.now();
  const mark = (step: string) => console.log(`FLOW-GUI ${step} ${Date.now() - started} ms`);

  await enter(page, env.pull);
  mark("entered");

  const checks = page.getByRole("region", { name: "Checks" });
  await expect(checks.getByText("passing", { exact: true })).toBeVisible();
  await expect(checks.getByText("success", { exact: true })).toBeVisible();
  await checks.getByRole("button", { name: /^Open check / }).first().click();
  const check = page.getByRole("dialog");
  await expect(check.getByText(env.log)).toBeVisible();
  await check.getByRole("button", { name: "Close" }).click();
  mark("checks-and-log-seen");

  await page.getByRole("button", { name: /file\(s\)/ }).first().click();
  const files = page.getByRole("dialog", { name: /Changed files/ });
  // A pull request of one file opens it at once; otherwise open the first.
  const first = files.locator("details.file").first();
  if ((await first.getAttribute("open")) === null) await first.locator("summary").click();
  await expect(files.locator("pre.patch")).toBeVisible();
  await files.getByRole("button", { name: "Close" }).click();
  mark("diff-seen");

  await page.getByRole("button", { name: "Merge (fast-forward)" }).click();
  await expect(page.getByText("Merged", { exact: true }).first()).toBeVisible();
  await expect(page.getByRole("button", { name: "Merge (fast-forward)" })).toHaveCount(0);
  mark("merged");

  await page.getByLabel("Related tickets").getByRole("link").first().click();
  await expect(page.getByText("Closed", { exact: true }).first()).toBeVisible();
  await expect(page.getByRole("combobox", { name: "Lane" })).toHaveValue("Done");
  mark("ticket-closed-and-done");
});
