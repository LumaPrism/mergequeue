"use client";

/* The control plane — a live signal box. GitHub-only auth gate → real data:
   /api/me (session), /api/repos (switcher), /api/repos/:id/prs (open PRs),
   /api/repos/:id/queue (the train). Drag a PR onto the train to add it; drag cars
   to reorder — the connection order is the merge order (GitLab-style merge train).
   The rail flows on real state, cars carry PR identity, and tags split-flap. */

import { useEffect, useRef, useState } from "react";
import type { CSSProperties, ReactNode } from "react";
import { createPortal } from "react-dom";
import {
  DndContext,
  DragOverlay,
  KeyboardSensor,
  PointerSensor,
  closestCenter,
  useDraggable,
  useDroppable,
  useSensor,
  useSensors,
} from "@dnd-kit/core";
import type { DragEndEvent, DragStartEvent } from "@dnd-kit/core";
import {
  SortableContext,
  arrayMove,
  sortableKeyboardCoordinates,
  useSortable,
  verticalListSortingStrategy,
} from "@dnd-kit/sortable";
import { CSS } from "@dnd-kit/utilities";

import { ActiveBatch } from "@/components/ActiveBatch";
import { DeparturesBoard } from "@/components/RunHistory";
import { PlatformSwitcher } from "@/components/PlatformSwitcher";
import { SetupGate } from "@/components/SetupGate";
import {
  ApiError,
  PrStatus,
  createQueue,
  dequeue,
  enqueue,
  getLedger,
  getMe,
  getOpenPrs,
  getQueue,
  getQueues,
  getRepos,
  reorder,
} from "@/lib/api";
import type { EntryView, LedgerView, MeView, PrView, QueueView, RepoView } from "@/lib/api";
import { relTime } from "@/lib/rel-time";
import { stateColor, statusColor, statusInk, svar } from "@/lib/state";

const stop = (e: { stopPropagation: () => void }) => e.stopPropagation();

/// A failed enqueue, captured for the confirm dialog. `fixable` errors (a 4xx the
/// backend explains — wrong base, already on another queue) show `message`
/// verbatim; everything else reads as a transient hiccup the user can retry.
type AddError = { fixable: boolean; message: string };

/// Loading placeholders sized to the real cars / PR cards, so swapping in content
/// causes no layout shift. Its own component — any column drops it in.
function Skeleton({ variant, count }: { variant: "car" | "pr"; count: number }) {
  return (
    <div className={variant === "car" ? "skel-train" : "skel-list"} aria-hidden>
      {Array.from({ length: count }).map((_, i) =>
        variant === "car" ? (
          <div className="skel-car" key={i}>
            <span className="skel skel-bar" style={{ width: "42%" }} />
          </div>
        ) : (
          <div className="skel-pr" key={i}>
            <span className="skel skel-bar" style={{ width: "55%" }} />
            <span className="skel skel-bar skel-bar-sm" style={{ width: "72%" }} />
          </div>
        ),
      )}
    </div>
  );
}

function LoginScreen() {
  return (
    <main className="login">
      <div className="login-card">
        {/* eslint-disable-next-line @next/next/no-img-element */}
        <img src="/logo.png" alt="" className="login-logo" />
        <h1>
          merge<b>queue</b>
        </h1>
        <p>Sign in with the GitHub account that administers your repositories.</p>
        <a className="login-btn" href="/auth/github/login">
          Log in with GitHub
          <span className="btn-arrow">→</span>
        </a>
        <a className="login-back" href="/">
          ← back to home
        </a>
      </div>
    </main>
  );
}

/// The signal-box's signature primitive: a state tag that *flips* (never fades)
/// when its value changes. Tabular board voice so columns lock.
function FlipTag({ value, className = "tag" }: { value: PrStatus; className?: string }) {
  const [shown, setShown] = useState<PrStatus>(value);
  const [flipping, setFlipping] = useState(false);

  useEffect(() => {
    if (value === shown) return;
    setFlipping(true);
    const mid = window.setTimeout(() => setShown(value), 90);
    const end = window.setTimeout(() => setFlipping(false), 180);
    return () => {
      window.clearTimeout(mid);
      window.clearTimeout(end);
    };
  }, [value, shown]);

  return (
    <span
      className={`${className} flip ${flipping ? "flipping" : ""}`}
      style={svar(statusColor[shown], statusInk[shown])}
    >
      <span className="flip-face">{shown}</span>
    </span>
  );
}

/// A listbox repo switcher with full keyboarding (Up/Down/Home/End/Enter/Escape,
/// roving aria-activedescendant) and focus restore.
function RepoSelect({
  repos,
  sel,
  onSelect,
}: {
  repos: RepoView[];
  sel: string | null;
  onSelect: (id: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(0);
  const btnRef = useRef<HTMLButtonElement>(null);
  const current = repos.find((r) => r.id === sel);
  const depthOf = (r: RepoView) => r.queues.reduce((n, q) => n + q.depth, 0);
  const tracksOf = (r: RepoView) =>
    `${r.queues.length} ${r.queues.length === 1 ? "track" : "tracks"}`;

  useEffect(() => {
    if (!open) return;
    const close = () => setOpen(false);
    window.addEventListener("click", close);
    return () => window.removeEventListener("click", close);
  }, [open]);

  useEffect(() => {
    if (open) setActive(Math.max(0, repos.findIndex((r) => r.id === sel)));
  }, [open, sel, repos]);

  const choose = (i: number) => {
    const r = repos[i];
    if (!r) return;
    onSelect(r.id);
    setOpen(false);
    btnRef.current?.focus();
  };

  const onKey = (e: React.KeyboardEvent) => {
    if (!open) {
      if (e.key === "ArrowDown" || e.key === "Enter" || e.key === " ") {
        e.preventDefault();
        setOpen(true);
      }
      return;
    }
    if (e.key === "Escape") {
      e.preventDefault();
      setOpen(false);
      btnRef.current?.focus();
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      setActive((a) => Math.min(repos.length - 1, a + 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setActive((a) => Math.max(0, a - 1));
    } else if (e.key === "Home") {
      e.preventDefault();
      setActive(0);
    } else if (e.key === "End") {
      e.preventDefault();
      setActive(repos.length - 1);
    } else if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      choose(active);
    }
  };

  return (
    <div className="rsel" onClick={stop}>
      <button
        ref={btnRef}
        type="button"
        className="rsel-btn"
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-activedescendant={open ? `rsel-opt-${active}` : undefined}
        onKeyDown={onKey}
        onClick={() => setOpen((o) => !o)}
      >
        <span className="rsel-current">
          {current ? `${current.owner}/${current.name}` : "select a repo"}
        </span>
        {current ? <span className="rsel-q">{depthOf(current)} queued</span> : null}
        <span className="rsel-caret" aria-hidden>
          ▾
        </span>
      </button>
      {open ? (
        <ul className="rsel-menu" role="listbox" aria-label="repositories">
          <li className="rsel-lbl" aria-hidden>
            repositories
          </li>
          {repos.map((r, i) => (
            <li
              key={r.id}
              id={`rsel-opt-${i}`}
              role="option"
              aria-selected={r.id === sel}
              className={`rsel-opt ${r.id === sel ? "on" : ""} ${i === active ? "active" : ""}`}
              style={{ ["--i"]: i } as CSSProperties}
              onMouseEnter={() => setActive(i)}
              onClick={() => choose(i)}
            >
              <span className="rsel-opt-main">
                <span className="rsel-opt-name">
                  {r.owner}/{r.name}
                </span>
                <span className="rsel-opt-base">{tracksOf(r)}</span>
              </span>
              <span className="rsel-opt-q">{depthOf(r)} queued</span>
            </li>
          ))}
        </ul>
      ) : null}
    </div>
  );
}

/// The inner content of a train car — carries real PR identity (board-voice
/// number, title, head→base branch) and a split-flap signal-aspect tag.
function CarBody({ entry, pr, href }: { entry: EntryView; pr?: PrView; href: string }) {
  const title = pr?.title ?? `PR #${entry.prNumber}`;
  return (
    <>
      <span className="knob" style={svar(statusColor[entry.status])} />
      <span className="num">
        <a
          className="prlink"
          href={href}
          target="_blank"
          rel="noreferrer"
          onPointerDown={stop}
          onKeyDown={stop}
        >
          #{entry.prNumber}
        </a>
      </span>
      <div className="body">
        <div className="ttl">{title}</div>
        {pr ? (
          <div className="who">
            {pr.headRef} → {pr.baseRef}
          </div>
        ) : null}
      </div>
      <FlipTag value={entry.status} />
    </>
  );
}

/// A queued car you can drag to reorder.
function Car({
  entry,
  pr,
  href,
  next,
  index,
  onRemove,
}: {
  entry: EntryView;
  pr?: PrView;
  href: string;
  next: boolean;
  index: number;
  onRemove: (e: EntryView) => void;
}) {
  const { attributes, listeners, setNodeRef, transform, transition, isDragging } = useSortable({
    id: entry.id,
    data: { type: "car", entry },
  });
  const style = {
    transform: CSS.Transform.toString(transform),
    transition,
    ["--i"]: index,
    ...svar(statusColor[entry.status]),
  } as CSSProperties;
  return (
    <div
      ref={setNodeRef}
      style={style}
      className={`node pr car ${next ? "next" : ""} ${isDragging ? "dragging" : ""}`}
      {...attributes}
      {...listeners}
    >
      <CarBody entry={entry} pr={pr} href={href} />
      <button
        type="button"
        className="car-x"
        aria-label={`remove #${entry.prNumber}`}
        onPointerDown={stop}
        onKeyDown={stop}
        onClick={(e) => {
          e.stopPropagation();
          onRemove(entry);
        }}
      >
        ×
      </button>
    </div>
  );
}

/// An open PR — a candidate to add to the train.
function PrCard({
  pr,
  queued,
  href,
  onAdd,
}: {
  pr: PrView;
  queued: boolean;
  href: string;
  onAdd: (pr: PrView) => void;
}) {
  const { attributes, listeners, setNodeRef, isDragging } = useDraggable({
    id: `pr:${pr.number}`,
    data: { type: "pr", pr },
    disabled: queued,
  });
  return (
    <li
      ref={setNodeRef}
      className={`pritem ${queued ? "is-queued" : "draggable"} ${isDragging ? "dragging" : ""}`}
      {...(queued ? {} : attributes)}
      {...(queued ? {} : listeners)}
    >
      <div className="pritem-main">
        <a
          className="prlink"
          href={href}
          target="_blank"
          rel="noreferrer"
          onPointerDown={stop}
          onKeyDown={stop}
        >
          #{pr.number}
        </a>
        <span className="pritem-ttl">{pr.title}</span>
      </div>
      <div className="pritem-meta">
        <span className="pritem-branch">
          {pr.headRef} → {pr.baseRef}
        </span>
        {queued ? (
          <span className="pritem-queued">on train</span>
        ) : (
          <button
            type="button"
            className="add-btn"
            onPointerDown={stop}
            onKeyDown={stop}
            onClick={() => onAdd(pr)}
          >
            Add
            <span className="add-plus" aria-hidden>
              +
            </span>
          </button>
        )}
      </div>
    </li>
  );
}

/// The train drop zone — opens a magnetic accent slot while a PR is dragged over.
function TrainZone({ active, children }: { active: boolean; children: ReactNode }) {
  const { setNodeRef, isOver } = useDroppable({ id: "train" });
  return (
    <div ref={setNodeRef} className={`trainzone ${active && isOver ? "over" : ""}`}>
      {children}
    </div>
  );
}

/// The queue confirm dialog — a real dialog contract: initial focus, Escape,
/// Tab focus-trap, and focus restore to the trigger on close.
function ConfirmModal({
  pr,
  repo,
  queue,
  busy,
  addErr,
  onCancel,
  onConfirm,
}: {
  pr: PrView;
  repo: RepoView;
  queue: QueueView;
  busy: boolean;
  addErr: AddError | null;
  onCancel: () => void;
  onConfirm: () => void;
}) {
  const dialogRef = useRef<HTMLDivElement>(null);
  const confirmRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    const prev = document.activeElement as HTMLElement | null;
    confirmRef.current?.focus();
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        onCancel();
        return;
      }
      if (e.key !== "Tab") return;
      const focusables = dialogRef.current?.querySelectorAll<HTMLElement>(
        'button:not([disabled]), [href], input, [tabindex]:not([tabindex="-1"])',
      );
      if (!focusables || focusables.length === 0) return;
      const first = focusables[0];
      const last = focusables[focusables.length - 1];
      if (e.shiftKey && document.activeElement === first) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && document.activeElement === last) {
        e.preventDefault();
        first.focus();
      }
    };
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("keydown", onKey);
      prev?.focus?.();
    };
  }, [onCancel]);

  return createPortal(
    <div className="modal-backdrop" onClick={onCancel}>
      <div
        ref={dialogRef}
        className="modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="mq-modal-title"
        aria-describedby="mq-modal-note"
        onClick={stop}
      >
        <h2 id="mq-modal-title" className="modal-title">
          Add #{pr.number} to the <b>{queue.name}</b> track?
        </h2>
        <p className="modal-pr">{pr.title}</p>
        <p id="mq-modal-note" className="modal-note">
          <b>mergequeue</b> stages it on a throwaway branch, waits for {repo.name}&apos;s required
          checks, and merges into <b>{queue.baseBranch}</b> if they pass. If the repo has no required
          checks, it&apos;s held — never merged ungated.
        </p>
        {addErr ? (
          <p className={`modal-error ${addErr.fixable ? "caution" : ""}`} role="alert">
            {addErr.fixable
              ? addErr.message
              : `Couldn’t add this PR — this is usually transient. Check the App is installed on ${repo.name}, then retry.`}
          </p>
        ) : null}
        <div className="modal-actions">
          <button type="button" className="modal-cancel" onClick={onCancel}>
            Cancel
          </button>
          <button
            ref={confirmRef}
            type="button"
            className="modal-confirm"
            disabled={busy}
            onClick={onConfirm}
          >
            {busy ? "Adding…" : addErr && !addErr.fixable ? "Retry" : "Add to train"}
          </button>
        </div>
      </div>
    </div>,
    document.body,
  );
}

export default function Dashboard() {
  const [me, setMe] = useState<MeView | null>(null);
  const [authed, setAuthed] = useState<boolean | null>(null);
  const [repos, setRepos] = useState<RepoView[]>([]);
  const [reposLoaded, setReposLoaded] = useState(false);
  const [minOver, setMinOver] = useState(false);
  const [prsReady, setPrsReady] = useState(false);
  const [sel, setSel] = useState<string | null>(null);
  const [queues, setQueues] = useState<QueueView[]>([]);
  const [selQueue, setSelQueue] = useState<string | null>(null);
  const [queue, setQueue] = useState<EntryView[]>([]);
  const [prs, setPrs] = useState<PrView[]>([]);
  const [prsErr, setPrsErr] = useState(false);
  const [queueErr, setQueueErr] = useState(false);
  const [busy, setBusy] = useState<number | null>(null);
  const [confirming, setConfirming] = useState<PrView | null>(null);
  const [addErr, setAddErr] = useState<AddError | null>(null);
  const [dragId, setDragId] = useState<string | null>(null);
  const [prFilter, setPrFilter] = useState("");
  const [undo, setUndo] = useState<{ entry: EntryView; index: number; queueId: string } | null>(
    null,
  );
  const [ledger, setLedger] = useState<LedgerView[]>([]);
  const [syncedAt, setSyncedAt] = useState<number | null>(null);
  const [now, setNow] = useState(() => Date.now());

  const sensors = useSensors(
    useSensor(PointerSensor, { activationConstraint: { distance: 6 } }),
    useSensor(KeyboardSensor, { coordinateGetter: sortableKeyboardCoordinates }),
  );

  const selRef = useRef(sel);
  selRef.current = sel;
  const selQueueRef = useRef(selQueue);
  selQueueRef.current = selQueue;

  // Repo-scoped data (PR candidates + the platform switcher's live tracks). Ignore
  // any response whose repo is no longer selected — a slow fetch for repo A must
  // never write A's data after the user has switched to B.
  const refreshRepoScope = (repoId: string) => {
    getOpenPrs(repoId)
      .then((p) => {
        if (repoId !== selRef.current) return;
        setPrs(p);
        setPrsErr(false);
        setPrsReady(true);
      })
      .catch(() => {
        if (repoId !== selRef.current) return;
        setPrs([]);
        setPrsErr(true);
        setPrsReady(true);
      });
    getQueues(repoId)
      .then((qs) => {
        if (repoId !== selRef.current) return;
        setQueues(qs);
      })
      .catch(() => {});
  };

  // Queue-scoped data (the train + the departures board). Guarded by the selected
  // queue id so a slow fetch for track A never lands after a switch to track B.
  const refreshQueueScope = (queueId: string) => {
    getQueue(queueId)
      .then((q) => {
        if (queueId !== selQueueRef.current) return;
        setQueue(q);
        setQueueErr(false);
        setSyncedAt(Date.now());
      })
      .catch(() => queueId === selQueueRef.current && setQueueErr(true));
    getLedger(queueId)
      .then((l) => {
        if (queueId !== selQueueRef.current) return;
        setLedger(l);
      })
      .catch(() => {});
  };

  // Refresh whatever is in view now (both scopes), reading the live refs.
  const refresh = () => {
    const r = selRef.current;
    const q = selQueueRef.current;
    if (r) refreshRepoScope(r);
    if (q) refreshQueueScope(q);
  };
  const refreshRef = useRef(refresh);
  refreshRef.current = refresh;
  const undoTimer = useRef<number | null>(null);
  const pauseRef = useRef(false);
  pauseRef.current = dragId !== null || confirming !== null || busy !== null || undo !== null;

  // Commit a removal to the backend against the track it was removed from, and only
  // refresh if we're still viewing that track (else we'd overwrite another's queue).
  const commitRemove = (queueId: string, entryId: string) => {
    dequeue(queueId, entryId)
      .catch(() => {})
      .finally(() => {
        if (queueId === selQueueRef.current) refreshRef.current();
      });
  };

  useEffect(() => {
    let alive = true;
    getMe()
      .then((u) => {
        if (!alive) return;
        setMe(u);
        setAuthed(u !== null);
      })
      .catch(() => alive && setAuthed(false));
    return () => {
      alive = false;
    };
  }, []);

  useEffect(() => {
    if (!authed) return;
    let alive = true;
    getRepos()
      .then((r) => {
        if (!alive) return;
        setRepos(r);
        setSel((s) => s ?? r[0]?.id ?? null);
        setReposLoaded(true);
      })
      .catch(() => alive && setReposLoaded(true));
    return () => {
      alive = false;
    };
  }, [authed]);

  // Repo switch: seed the platform switcher from the repo's embedded queues for an
  // instant paint, default-select its `default` track, then refetch repo-scoped data
  // (PR candidates + the live tracks with their active-batch aspect pips).
  useEffect(() => {
    if (!sel) return;
    const repo = repos.find((r) => r.id === sel);
    const seeded = repo?.queues ?? [];
    setQueues(seeded);
    const def = seeded.find((q) => q.name === "default") ?? seeded[0] ?? null;
    setSelQueue(def?.id ?? null);
    setPrs([]);
    setPrsErr(false);
    setPrsReady(false);
    refreshRepoScope(sel);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sel]);

  // Track switch: reset the train + departures board, then load the selected queue.
  useEffect(() => {
    if (!selQueue) return;
    setQueue([]);
    setSyncedAt(null);
    setQueueErr(false);
    setLedger([]);
    refreshQueueScope(selQueue);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selQueue]);

  // Keep the loading skeleton up for a brief minimum so a fast load doesn't flash
  // it for a frame — restarts on first auth and on every repo switch.
  useEffect(() => {
    if (authed !== true) return;
    setMinOver(false);
    const t = window.setTimeout(() => setMinOver(true), 350);
    return () => window.clearTimeout(t);
  }, [authed, sel]);

  useEffect(() => {
    if (!authed) return;
    const t = window.setInterval(() => {
      if (!pauseRef.current) refreshRef.current();
    }, 10000);
    return () => window.clearInterval(t);
  }, [authed]);

  useEffect(() => {
    const t = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(t);
  }, []);

  if (authed === null) return <main className="shell" />;
  if (!authed) return <LoginScreen />;

  const repo = repos.find((r) => r.id === sel) ?? null;
  const activeQueue = queues.find((q) => q.id === selQueue) ?? null;
  const baseBranch = activeQueue?.baseBranch;
  // Each column keeps its skeleton until ITS data is in — and for a brief minimum —
  // so neither flashes the empty state mid-load (queue and PRs resolve independently).
  const queueLoading = !minOver || !reposLoaded || syncedAt === null;
  const prsLoading = !minOver || !reposLoaded || !prsReady;
  const queuedNums = new Set(queue.map((e) => e.prNumber));
  const prByNum = new Map(prs.map((p) => [p.number, p] as const));
  const pinned = queue.filter((e) => e.status !== PrStatus.Queued);
  const cars = queue.filter((e) => e.status === PrStatus.Queued);
  const batchSize = activeQueue?.batchSize ?? 0;
  const filter = prFilter.trim().toLowerCase();
  const shownPrs = filter
    ? prs.filter((p) => `#${p.number} ${p.title} ${p.headRef}`.toLowerCase().includes(filter))
    : prs;

  const atMerge = (e: EntryView) =>
    e.status === PrStatus.Merged ||
    e.status === PrStatus.Merging ||
    e.status === PrStatus.Blocked;

  const railState = pinned.some((e) => e.status === PrStatus.Ejected)
    ? "ejecting"
    : pinned.some(atMerge)
      ? "merging"
      : pinned.some((e) => e.status === PrStatus.Testing)
        ? "testing"
        : "idle";

  const batchStage: "testing" | "merging" | "merged" | "ejected" | null =
    pinned.length === 0
      ? null
      : pinned.some((e) => e.status === PrStatus.Ejected)
        ? "ejected"
        : pinned.every((e) => e.status === PrStatus.Merged)
          ? "merged"
          : pinned.some(atMerge)
            ? "merging"
            : "testing";

  const batchHeadState: PrStatus | null = pinned.some(
    (e) => e.status === PrStatus.Blocked,
  )
    ? PrStatus.Blocked
    : batchStage === "testing"
      ? PrStatus.Testing
      : batchStage === "merging"
        ? PrStatus.Merging
        : batchStage === "merged"
          ? PrStatus.Merged
          : batchStage === "ejected"
            ? PrStatus.Ejected
            : null;

  const ghUrl = (n: number) =>
    repo ? `https://github.com/${repo.owner}/${repo.name}/pull/${n}` : "#";

  const createTrack = async (name: string) => {
    if (!sel) return;
    const created = await createQueue(sel, { name });
    const qs = await getQueues(sel);
    if (sel !== selRef.current) return;
    setQueues(qs);
    setSelQueue(created.id);
  };

  const doQueue = async (pr: PrView) => {
    if (!selQueue) return;
    setBusy(pr.number);
    setAddErr(null);
    const tempId = `tmp:${pr.number}`;
    setQueue((q) =>
      q.some((e) => e.prNumber === pr.number)
        ? q
        : [...q, { id: tempId, prNumber: pr.number, position: q.length, status: PrStatus.Queued }],
    );
    try {
      await enqueue(selQueue, pr.number);
      setConfirming(null);
      refresh();
    } catch (err) {
      setQueue((q) => q.filter((e) => e.id !== tempId));
      // 409 AlreadyQueued / 422 WrongBase carry a message the user can act on;
      // show it verbatim. Anything else (5xx, network) reads as transient.
      setAddErr(
        err instanceof ApiError
          ? { fixable: err.userFixable, message: err.message }
          : { fixable: false, message: "" },
      );
    } finally {
      setBusy(null);
    }
  };

  const doRemove = (entry: EntryView) => {
    if (!selQueue) return;
    // Only the latest removal is undoable. If one is still pending, commit it now
    // (against ITS track) so its dequeue is never dropped by the new one.
    if (undo && undoTimer.current !== null) {
      window.clearTimeout(undoTimer.current);
      commitRemove(undo.queueId, undo.entry.id);
    }
    const queueId = selQueue;
    const index = Math.max(0, queue.findIndex((e) => e.id === entry.id));
    setQueue((q) => q.filter((e) => e.id !== entry.id));
    setUndo({ entry, index, queueId });
    undoTimer.current = window.setTimeout(() => {
      undoTimer.current = null;
      setUndo(null);
      commitRemove(queueId, entry.id);
    }, 5000);
  };

  const undoRemove = () => {
    // Don't splice a removal from another track into the one now on screen.
    if (!undo || undo.queueId !== selQueueRef.current) return;
    if (undoTimer.current !== null) {
      window.clearTimeout(undoTimer.current);
      undoTimer.current = null;
    }
    setQueue((q) => {
      if (q.some((e) => e.id === undo.entry.id)) return q;
      const next = [...q];
      next.splice(Math.min(undo.index, next.length), 0, undo.entry);
      return next;
    });
    setUndo(null);
  };

  const onDragStart = (e: DragStartEvent) => setDragId(String(e.active.id));

  const onDragEnd = (e: DragEndEvent) => {
    setDragId(null);
    const { active, over } = e;
    if (!over || !selQueue) return;
    const type = active.data.current?.type;
    if (type === "pr") {
      setConfirming(active.data.current?.pr as PrView);
      return;
    }
    if (type === "car" && active.id !== over.id) {
      const oldIndex = cars.findIndex((c) => c.id === active.id);
      const newIndex = cars.findIndex((c) => c.id === over.id);
      if (oldIndex < 0 || newIndex < 0) return;
      const reordered = arrayMove(cars, oldIndex, newIndex);
      // A car removed in the undo window is still on the backend, so the reorder
      // response would include it — filter it out so it doesn't reappear.
      const removedId = undo?.entry.id;
      setQueue([...pinned, ...reordered]);
      reorder(
        selQueue,
        reordered.map((c) => c.id),
      )
        .then((q) => setQueue(removedId ? q.filter((e) => e.id !== removedId) : q))
        .catch(() => refresh());
    }
  };

  const draggingPr = dragId?.startsWith("pr:") ?? false;
  const dragPr = draggingPr ? prs.find((p) => `pr:${p.number}` === dragId) : null;
  const dragCar = dragId && !draggingPr ? queue.find((c) => c.id === dragId) : null;
  return (
    <main className="shell" inert={confirming ? true : undefined}>
      <header className="topbar">
        <div className="brand">
          {/* eslint-disable-next-line @next/next/no-img-element */}
          <img src="/logo.png" alt="mergequeue" className="logo" />
          <span className="tagline">control plane</span>
        </div>
        {me && (
          <div className="userchip">
            {me.avatarUrl ? (
              // eslint-disable-next-line @next/next/no-img-element
              <img src={me.avatarUrl} alt="" className="userav" />
            ) : null}
            <span>@{me.login}</span>
            <a className="logout" href="/auth/logout">
              Sign out
            </a>
          </div>
        )}
      </header>

      <SetupGate />

      {reposLoaded && repos.length === 0 ? (
        <div className="emptyrepos">
          <h3>No repositories yet</h3>
          <p>
            Register the GitHub App above, then install it on a repository — it&apos;ll appear here
            and you can start building a merge train.
          </p>
        </div>
      ) : (
        <>
          <section className="sb-head">
            <div className="sb-head-top">
              <RepoSelect repos={repos} sel={sel} onSelect={(id) => setSel(id)} />
              {repo && (
                <span className="live" role="status" aria-live="polite">
                  <span className="dot" aria-hidden />
                  <span className="live-k">live</span>
                  <span className="live-ago">updated {relTime(syncedAt, now)}</span>
                </span>
              )}
            </div>
            <h1 className="sb-title">
              <span className="sb-title-k">Signal box</span>
              {repo ? (
                <span className="sb-title-repo">
                  {repo.owner}/{repo.name}
                </span>
              ) : null}
            </h1>
          </section>

          {repo && queues.length > 0 ? (
            <PlatformSwitcher
              queues={queues}
              selected={selQueue}
              onSelect={(id) => setSelQueue(id)}
              onCreate={createTrack}
            />
          ) : null}

          {activeQueue && (
            <div className="sb-ctx">
              <span className="sb-ctx-item">
                <span className="sb-ctx-k">base</span>
                <b>{activeQueue.baseBranch}</b>
              </span>
              <span className="sb-ctx-item">
                <span className="sb-ctx-k">batch</span>
                <b>{activeQueue.batchSize}</b>
              </span>
              <span className="sb-ctx-cap">
                Queued PRs ride the <b>{activeQueue.name}</b> track; cars land into{" "}
                {activeQueue.baseBranch} in merge order.
              </span>
            </div>
          )}

          <DndContext
            sensors={sensors}
            collisionDetection={closestCenter}
            onDragStart={onDragStart}
            onDragEnd={onDragEnd}
          >
            <div className="flow">
              <div className="stack" data-rail={railState}>
                <div className="rail">
                  <span className="surge" />
                </div>

                <div className="node dest" style={{ ...svar(stateColor.merged), ["--i"]: 0 } as CSSProperties}>
                  <span className="knob full" style={svar(stateColor.merged)} />
                  <span className="label">{baseBranch ?? "main"}</span>
                  <span className="meta">green cars land here</span>
                </div>

                {queueErr && queue.length === 0 ? (
                  <div className="errcard" role="alert">
                    <span className="errcard-msg">Couldn&apos;t load the train.</span>
                    <button type="button" className="errcard-btn" onClick={() => refresh()}>
                      Retry
                    </button>
                  </div>
                ) : queueLoading ? (
                  <Skeleton variant="car" count={3} />
                ) : (
                  <>
                    {pinned.length > 0 && (
                      <ActiveBatch
                        pinned={pinned}
                        head={batchHeadState}
                        stage={batchStage}
                        prByNum={prByNum}
                        ghUrl={ghUrl}
                        baseBranch={baseBranch}
                        onRemove={doRemove}
                      />
                    )}
                    <TrainZone active={draggingPr}>
                      {queue.length === 0 ? (
                        <div className="qempty">
                          <span className="qempty-title">
                            {prs.length === 0 ? "Nothing to board yet" : "The train is empty"}
                          </span>
                          <span className="qempty-sub">
                            {prs.length === 0
                              ? "Open a pull request, then add it here to start the train."
                              : `Add a ready PR to start the train — cars land into ${baseBranch ?? "main"} in order.`}
                          </span>
                          {prs.length > 0 && shownPrs[0] ? (
                            <button
                              type="button"
                              className="qempty-add"
                              onClick={() => setConfirming(shownPrs[0])}
                            >
                              Add #{shownPrs[0].number}
                            </button>
                          ) : null}
                        </div>
                      ) : (
                        <>
                          {cars.length > 0 && (
                            <div className="departure">
                              <span className="departure-k">next departure</span>
                              <span className="departure-n">
                                {Math.min(batchSize || cars.length, cars.length)} cars
                              </span>
                              <span className="departure-rule" aria-hidden />
                            </div>
                          )}
                          <SortableContext
                            items={cars.map((c) => c.id)}
                            strategy={verticalListSortingStrategy}
                          >
                            {cars.map((e, i) => (
                              <Car
                                key={e.id}
                                entry={e}
                                pr={prByNum.get(e.prNumber)}
                                href={ghUrl(e.prNumber)}
                                next={i < batchSize}
                                index={pinned.length + i + 1}
                                onRemove={doRemove}
                              />
                            ))}
                          </SortableContext>
                        </>
                      )}
                    </TrainZone>
                  </>
                )}
              </div>

              <aside className="side">
                <div className="card">
                  <div className="card-head">
                    <h2 className="card-title">open pull requests</h2>
                    <span className="card-sub" role="status" aria-live="polite">
                      {prsLoading ? "—" : filter ? `${shownPrs.length}/${prs.length}` : prs.length}
                    </span>
                  </div>
                  {prsErr ? (
                    <div className="detail-sub">
                      Couldn&apos;t load PRs — the App may still be warming up its token. Refresh in a
                      moment.
                    </div>
                  ) : prsLoading ? (
                    <Skeleton variant="pr" count={6} />
                  ) : prs.length === 0 ? (
                    <div className="detail-sub">No open PRs on {repo?.name} right now.</div>
                  ) : (
                    <>
                      <input
                        className="prsearch"
                        type="search"
                        placeholder="filter by #, title or branch…"
                        value={prFilter}
                        onChange={(e) => setPrFilter(e.target.value)}
                      />
                      {shownPrs.length === 0 ? (
                        <div className="detail-sub" role="status">
                          No PRs match “{prFilter}”.
                        </div>
                      ) : (
                        <ul className="prlist">
                          {shownPrs.map((p) => (
                            <PrCard
                              key={p.number}
                              pr={p}
                              queued={queuedNums.has(p.number)}
                              href={ghUrl(p.number)}
                              onAdd={(pr) => setConfirming(pr)}
                            />
                          ))}
                        </ul>
                      )}
                    </>
                  )}
                </div>
              </aside>
            </div>

            <DeparturesBoard rows={ledger} track={activeQueue?.name} loading={queueLoading} />

            <DragOverlay>
              {dragPr ? (
                <div className="drag-ghost">
                  <span className="drag-num">#{dragPr.number}</span>
                  <span className="drag-ttl">{dragPr.title}</span>
                </div>
              ) : dragCar ? (
                <div className="drag-ghost car">
                  <span className="drag-num">#{dragCar.prNumber}</span>
                  <span className="drag-ttl">
                    {prByNum.get(dragCar.prNumber)?.title ?? "reordering…"}
                  </span>
                </div>
              ) : null}
            </DragOverlay>
          </DndContext>
        </>
      )}

      <footer className="foot">
        <span>mergequeue · ci-agnostic merge train</span>
        <a href="/docs">docs</a>
        <span style={{ marginLeft: "auto", color: "var(--text-meta)" }}>self-hosted</span>
      </footer>

      {confirming && repo && activeQueue ? (
        <ConfirmModal
          pr={confirming}
          repo={repo}
          queue={activeQueue}
          busy={busy === confirming.number}
          addErr={addErr}
          onCancel={() => {
            setConfirming(null);
            setAddErr(null);
          }}
          onConfirm={() => doQueue(confirming)}
        />
      ) : null}

      {undo && undo.queueId === selQueue
        ? createPortal(
            <div className="toast" role="status" aria-live="polite">
              <span className="toast-msg">Removed #{undo.entry.prNumber} from the train</span>
              <button type="button" className="toast-undo" onClick={undoRemove}>
                Undo
              </button>
            </div>,
            document.body,
          )
        : null}
    </main>
  );
}
