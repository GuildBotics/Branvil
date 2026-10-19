// Server-Sent Events (`GET /api/v1/events`) → query invalidation.
//
// Events only say what changed; the GUI refetches the affected queries.
// `invalidateQueries` refetches only queries that are shown (active) and marks
// the others stale, so an event never fans out into fetching hidden data.

import type { Query, QueryClient } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import type { Card, ChangeEvent, Checks, PullRequest } from "../api/types";
import { routeOfUrl } from "../router/routes";

type Predicate = (query: Query) => boolean;

const keyOf = (q: Query) => q.queryKey as readonly unknown[];

const isCards = (q: Query) => keyOf(q)[0] === "board" && keyOf(q)[2] === "cards";
/** Paths name the same item whatever their case (the service compares them case-insensitively). */
const samePath = (key: unknown, path: string) => typeof key === "string" && key.toLowerCase() === path.toLowerCase();

function boardCardsWhere(test: (cards: Card[]) => boolean): Predicate {
  return (q) => isCards(q) && test((q.state.data as { items: Card[] } | undefined)?.items ?? []);
}

const isRepo = (q: Query, repo: string) => keyOf(q)[0] === "repo" && samePath(keyOf(q)[1], repo);

/** The pull request number of a repository query (`["repo", repo, "pull", n, ...]`). */
const pullNumberOf = (q: Query) => (keyOf(q)[2] === "pull" ? String(keyOf(q)[3]) : null);

/**
 * The queries a change event makes stale (docs/api.md, "Events"):
 * `ticket` repository/number, `pull` repository/number, `board` workspace, `check`
 * repository/commit sha, `upstream` repository (what following its upstream
 * found), `repo` repository.
 * `null` means "everything".
 */
export function predicatesFor(event: ChangeEvent, client: QueryClient): Predicate[] | null {
  const path = event.path;
  const id = event.id ?? "";
  if (!path) return null;
  switch (event.kind) {
    case "ticket":
      return [
        (q) => isRepo(q, path) && keyOf(q)[2] === "ticket" && (id === "" || keyOf(q)[3] === id),
        // The board of the repository's workspace may show the ticket (or show
        // it again once it is reopened): only the shown cards are refetched.
        (q) => isCards(q) && samePath(keyOf(q)[1], path.split("/")[0] ?? ""),
      ];
    case "board":
      return [(q) => keyOf(q)[0] === "board" && samePath(keyOf(q)[1], path)];
    case "pull":
      return [
        // That pull request only (its diff is keyed by the head sha), and the repository's PR list.
        (q) => isRepo(q, path) && (id === "" || pullNumberOf(q) === id) && keyOf(q)[4] !== "files",
        (q) => isRepo(q, path) && keyOf(q)[2] === "pull-list",
        (q) => keyOf(q)[2] === "ticket" && keyOf(q)[4] === "pulls",
        boardCardsWhere((cards) =>
          cards.some((c) =>
            c.pull_requests.some((p) => {
              const r = routeOfUrl(p.url);
              return r?.name === "pull" && r.repo === path && (id === "" || String(r.number) === id);
            }),
          ),
        ),
      ];
    case "check":
      return [
        (q) => {
          if (!isRepo(q, path)) return false;
          const k = keyOf(q);
          const sha = (q.state.data as { sha?: string } | undefined)?.sha;
          if (k[2] === "job") return id === "" || sha === id;
          // Before its first answer, a commit's checks are known by the sha in the key.
          if (k[2] === "commit" && k[4] === "checks") return id === "" || (sha ?? k[3]) === id;
          if (k[2] !== "pull" || k[4] !== "checks") return false;
          if (id === "") return true;
          const checks = q.state.data as Checks | undefined;
          const pull = client.getQueryData<PullRequest>(["repo", path, "pull", k[3]]);
          return checks?.head_sha === id || pull?.head_sha === id;
        },
      ];
    case "repo":
      // A push moves branch tips, so the pull requests of the repository are re-read too,
      // and new required checks change every commit's checks; its tickets and commits do not change.
      return [
        (q) => keyOf(q)[0] === "repos",
        (q) => {
          const k = keyOf(q);
          return isRepo(q, path) && k[4] !== "files" && k[2] !== "ticket" && (k[2] !== "commit" || k[4] === "checks");
        },
      ];
    case "upstream":
      return [
        (q) => keyOf(q)[0] === "repos",
        (q) => isRepo(q, path) && keyOf(q).length === 2,
        (q) => keyOf(q)[0] === "diagnostics",
      ];
    default:
      return null;
  }
}

export function applyChange(event: ChangeEvent, client: QueryClient): void {
  const predicates = predicatesFor(event, client);
  if (predicates === null) {
    void client.invalidateQueries();
    return;
  }
  void client.invalidateQueries({ predicate: (q) => predicates.some((p) => p(q)) });
}

const MAX_BACKOFF_MS = 30_000;
/** How long the stream may be down before the GUI warns: a moment's drop (waking from sleep, a restart of the service) is not worth one. */
export const LOST_AFTER_MS = 5_000;

/**
 * Keeps one EventSource open while `enabled`. On every (re)open and on
 * `reset`, refetches what is shown so a missed event cannot leave the view
 * stale; reconnects with a bounded exponential backoff. Returns whether the
 * stream has been down for `LOST_AFTER_MS`, the view possibly stale since.
 */
export function useChangeStream(client: QueryClient, enabled: boolean): boolean {
  const [lost, setLost] = useState(false);

  useEffect(() => {
    if (!enabled || typeof EventSource === "undefined") return;
    let source: EventSource | null = null;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let lostTimer: ReturnType<typeof setTimeout> | undefined;
    let backoff = 1000;
    let opened = false;
    let stopped = false;

    const refetchShown = () => void client.invalidateQueries();

    const connect = () => {
      source = new EventSource("/api/v1/events", { withCredentials: true });
      source.onopen = () => {
        backoff = 1000;
        clearTimeout(lostTimer);
        lostTimer = undefined;
        setLost(false);
        // The first open follows the initial fetches; later opens are reconnects.
        if (opened) refetchShown();
        opened = true;
      };
      source.addEventListener("change", (e) => {
        try {
          applyChange(JSON.parse((e as MessageEvent<string>).data) as ChangeEvent, client);
        } catch {
          refetchShown();
        }
      });
      source.addEventListener("reset", refetchShown);
      source.onerror = () => {
        lostTimer ??= setTimeout(() => setLost(true), LOST_AFTER_MS);
        if (source && source.readyState === EventSource.CLOSED && !stopped) {
          source.close();
          timer = setTimeout(connect, backoff);
          backoff = Math.min(backoff * 2, MAX_BACKOFF_MS);
        }
      };
    };

    connect();
    return () => {
      stopped = true;
      clearTimeout(timer);
      clearTimeout(lostTimer);
      source?.close();
    };
  }, [client, enabled]);

  return lost;
}
