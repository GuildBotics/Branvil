// Temporary measurement for #78: preserve RTL's wrapper and use a captured
// clock before tests install fake timers.
import { configure, getConfig } from "@testing-library/dom";
import { beforeEach } from "vitest";

const realNow = performance.now.bind(performance);
type Call = { ms: number; stack: string };
let calls: Call[] = [];
beforeEach(({ task }) => {
  calls = [];
  Object.assign(task.meta, { asyncCalls: calls });
});

const original = getConfig().asyncWrapper;
configure({ asyncWrapper: async (callback) => {
  const recorded = calls;
  // user-event also uses this wrapper; retain the callsite to separate it
  // from waitForWrapper (including findBy*) when analysing the report.
  const stack = new Error().stack ?? "";
  const started = realNow();
  try {
    return await original(callback);
  } finally {
    recorded.push({ ms: realNow() - started, stack });
  }
} });
