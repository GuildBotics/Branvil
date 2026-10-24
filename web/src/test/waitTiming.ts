// Temporary measurement for #78: preserve RTL's wrapper and use a captured
// clock before tests install fake timers.
import { configure, getConfig } from "@testing-library/dom";
import { afterAll, beforeEach } from "vitest";

const realNow = performance.now.bind(performance);
type Call = { ms: number; stack: string; failed: boolean };
let calls: Call[] = [];
beforeEach(({ task }) => {
  calls = [];
  Object.assign(task.meta, { asyncCalls: calls });
});

const original = getConfig();
configure({ asyncUtilTimeout: 5_000, asyncWrapper: async (callback) => {
  const recorded = calls;
  // user-event also uses this wrapper; retain the callsite to separate it
  // from waitForWrapper (including findBy*) when analysing the report.
  const stack = new Error().stack ?? "";
  const started = realNow();
  let failed = true;
  try {
    const result = await original.asyncWrapper(callback);
    failed = false;
    return result;
  } finally {
    recorded.push({ ms: realNow() - started, stack, failed });
  }
} });
afterAll(() => configure({ asyncWrapper: original.asyncWrapper, asyncUtilTimeout: original.asyncUtilTimeout }));
