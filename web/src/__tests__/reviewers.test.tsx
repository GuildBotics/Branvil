import { MantineProvider } from "@mantine/core";
import { QueryClientProvider } from "@tanstack/react-query";
import { act, render, screen, waitFor, within } from "@testing-library/react";
import type { ReactNode } from "react";
import { describe, expect, it } from "vitest";
import type { Activity, Review } from "../api/types";
import { I18nProvider } from "../i18n";
import { createQueryClient } from "../query/hooks";
import { keys } from "../query/keys";
import { apiError, fakeApi, json } from "../test/fakeApi";
import { ReviewersCell } from "../views/pulls/ReviewersCell";
import { BASE, HEAD, NEW_HEAD, pull } from "./fixtures";

const P = "/api/v1/repos/acme/demo/pulls/3";
const review = (author: string, verdict: Review["verdict"], minute: number, extra: Partial<Review> = {}): Review => ({
  id: `${author}-${minute}`,
  author,
  verdict,
  commit: HEAD,
  body: "",
  submitted_at: `2026-10-14T10:${String(minute).padStart(2, "0")}:00.000Z`,
  reactions: [],
  ...extra,
});

function renderReviewers(pr = pull(), reviews: Review[] = [], activityReply?: Parameters<typeof fakeApi>[0][string]) {
  const activity: Activity = { comments: [], reviews, threads: [] };
  const fake = fakeApi({
    [`GET ${P}/activity`]: activityReply ?? json(200, activity),
    "GET /api/v1/members": json(200, { items: [], next_cursor: null }),
  });
  const client = createQueryClient();
  const wrapper = ({ children }: { children: ReactNode }) => (
    <MantineProvider env="test">
      <QueryClientProvider client={client}>
        <I18nProvider initial="en">{children}</I18nProvider>
      </QueryClientProvider>
    </MantineProvider>
  );
  return { ...render(<a href="/acme/demo/-/pulls/3"><ReviewersCell pull={pr} /></a>, { wrapper }), fake, client };
}

describe("pull request list reviewers", () => {
  it("waits for activity before saying that a pull request has no reviewers", async () => {
    let finish!: (reply: ReturnType<typeof json>) => void;
    const response = new Promise<ReturnType<typeof json>>((resolve) => { finish = resolve; });
    const { container } = renderReviewers(pull({ reviewers: [] }), [], () => response);
    const loading = screen.getByRole("img", { name: "Loading…" });
    expect(loading).toHaveAttribute("title", "Loading…");
    expect(screen.queryByRole("img", { name: "Reviewers: No one" })).not.toBeInTheDocument();
    expect(container.querySelector(".review-mark-anchor")).not.toBeInTheDocument();
    await act(async () => finish(json(200, { comments: [], reviews: [], threads: [] })));
    expect(await screen.findByRole("img", { name: "Reviewers: No one" })).toHaveTextContent("—");
  });

  it.each([{ reviewers: [] }, { reviewers: ["requested-agent"] }])("does not infer review status from requested reviewers after a 403 (%j)", async ({ reviewers }) => {
    const { container } = renderReviewers(pull({ reviewers }), [], apiError(403, "forbidden", "Unavailable"));
    const unavailable = await screen.findByRole("img", { name: "Review status unavailable" });
    expect(unavailable).toHaveAttribute("title", "Review status unavailable");
    expect(container.querySelector(".review-mark-anchor")).not.toBeInTheDocument();
    expect(screen.queryByRole("img", { name: /Reviewers:/ })).not.toBeInTheDocument();
  });

  it("hides cached review marks when refreshing activity fails", async () => {
    const reviews = [review("review-agent", "approve", 1)];
    const { container, client } = renderReviewers(pull({ reviewers: ["review-agent"] }), reviews, [
      json(200, { comments: [], reviews, threads: [] }),
      apiError(403, "forbidden", "Unavailable"),
    ]);
    await screen.findByRole("img", { name: "Reviewers: review-agent (Requested again)" });
    expect(container.querySelector(".review-mark.pending")).toBeInTheDocument();
    await act(() => client.invalidateQueries({ queryKey: keys.pullPart("acme/demo", 3, "activity") }));
    expect(await screen.findByRole("img", { name: "Review status unavailable" })).toHaveAttribute("title", "Review status unavailable");
    expect(client.getQueryData<Activity>(keys.pullPart("acme/demo", 3, "activity"))?.reviews).toEqual(reviews);
    expect(container.querySelector(".review-mark-anchor")).not.toBeInTheDocument();
    expect(screen.queryByRole("img", { name: /Reviewers:/ })).not.toBeInTheDocument();
  });

  it("shows the latest verdict, stale verdicts and new requests with the detail view's marks", async () => {
    const { container } = renderReviewers(pull({ reviewers: ["pending-agent", "again-agent"] }), [
      review("approved-agent", "approve", 3),
      review("approved-agent", "request_changes", 1, { commit: BASE }),
      review("comment-agent", "comment", 2, { commit: BASE }),
      review("changes-agent", "request_changes", 4),
      review("again-agent", "approve", 5, { commit: BASE }),
      review("approved-agent", "comment", 6, { reply_only: true }),
      review("reply-agent", "comment", 7, { reply_only: true }),
    ]);
    const summary = await screen.findByRole("img", {
      name: "Reviewers: approved-agent (Approved), comment-agent (Commented (older revision)), changes-agent (Changes requested), again-agent (Requested again), pending-agent (Requested)",
    });
    expect(summary).toHaveAttribute("title", summary.getAttribute("aria-label"));
    expect(container.querySelectorAll(".review-mark.approve")).toHaveLength(1);
    expect(container.querySelectorAll(".review-mark.comment.stale")).toHaveLength(1);
    expect(container.querySelectorAll(".review-mark.request_changes")).toHaveLength(1);
    expect(container.querySelectorAll(".review-mark.pending")).toHaveLength(2);
    expect(container.querySelectorAll(".review-mark.stale")).toHaveLength(1);
    expect(container.querySelectorAll(".review-mark-anchor")).toHaveLength(5);
    // The row is the only navigation target; avatars add no interactive controls.
    const link = screen.getByRole("link");
    expect(within(link).queryByRole("button")).not.toBeInTheDocument();
    expect(within(link).queryByRole("link")).not.toBeInTheDocument();
    expect(container.querySelector("[tabindex]")).not.toBeInTheDocument();
  });

  it("loads reviews for closed pull requests and recalculates stale marks when their head changes", async () => {
    const { container, fake, rerender } = renderReviewers(pull({ state: "closed", reviewers: [] }), [review("review-agent", "approve", 1)]);
    await screen.findByRole("img", { name: "Reviewers: review-agent (Approved)" });
    expect(container.querySelector(".review-mark.approve")).not.toHaveClass("stale");
    rerender(<a href="/acme/demo/-/pulls/3"><ReviewersCell pull={pull({ state: "closed", reviewers: [], head_sha: NEW_HEAD })} /></a>);
    await screen.findByRole("img", { name: "Reviewers: review-agent (Approved (older revision))" });
    expect(container.querySelector(".review-mark.approve")).toHaveClass("stale");
    expect(fake.calls("GET", `${P}/activity`)).toHaveLength(1);
  });

  it("shows no reviewer avatar for reply-only participation", async () => {
    const { container, client } = renderReviewers(pull({ reviewers: [] }), [review("reply-agent", "comment", 1, { reply_only: true })]);
    await waitFor(() => expect(client.getQueryState(keys.pullPart("acme/demo", 3, "activity"))?.status).toBe("success"));
    expect(await screen.findByRole("img", { name: "Reviewers: No one" })).toHaveTextContent("—");
    expect(container.querySelector(".review-mark-anchor")).not.toBeInTheDocument();
  });
});
