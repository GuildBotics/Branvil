// A minimal History API router: the GUI has few routes and its URL grammar
// (`/{path}/-/{kind}/{id}`) is parsed by routes.ts.

import { useSyncExternalStore, type AnchorHTMLAttributes, type MouseEvent, type ReactNode } from "react";
import { inAppPath } from "./routes";

const listeners = new Set<() => void>();

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  window.addEventListener("popstate", listener);
  return () => {
    listeners.delete(listener);
    window.removeEventListener("popstate", listener);
  };
}

const snapshot = () => window.location.pathname;

export function navigate(to: string, opts: { replace?: boolean } = {}): void {
  if (opts.replace) window.history.replaceState(null, "", to);
  else window.history.pushState(null, "", to);
  for (const l of listeners) l();
  if (!to.includes("#")) window.scrollTo?.(0, 0);
}

/** Leaves the GUI for another address (a local app's redirect). */
export function leave(url: string): void {
  window.location.assign(url);
}

export function usePathname(): string {
  return useSyncExternalStore(subscribe, snapshot, snapshot);
}

type LinkProps = AnchorHTMLAttributes<HTMLAnchorElement> & { to: string; children: ReactNode; go?: () => void };

/** An in-app link: plain left clicks navigate without reloading the page (or `go` instead), others open `to` as a link does. */
export function Link({ to, children, onClick, go = () => navigate(to), ...rest }: LinkProps) {
  const handle = (e: MouseEvent<HTMLAnchorElement>) => {
    onClick?.(e);
    if (e.defaultPrevented || e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
    e.preventDefault();
    go();
  };
  return (
    <a href={to} onClick={handle} {...rest}>
      {children}
    </a>
  );
}

/** A link for a `url` the service returned: in-app when it is ours, external otherwise. */
export function ServiceLink({ url, children, className }: { url: string; children: ReactNode; className?: string }) {
  const path = inAppPath(url);
  if (path !== null) {
    return (
      <Link to={path} className={className}>
        {children}
      </Link>
    );
  }
  // Foreign references are stored as given; only http(s) becomes a link.
  if (!/^https?:\/\//i.test(url)) return <span className={className}>{children}</span>;
  return (
    <a href={url} className={className} target="_blank" rel="noopener noreferrer">
      {children}
    </a>
  );
}
