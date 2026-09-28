import { ArrowLeft, LockKeyhole, MessageCircle, RefreshCw } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { AgentEntry } from "nanocodex-react/agent";
import { TerminalComposer } from "nanocodex-terminal/composer";
import { TerminalTranscriptSurface } from "nanocodex-terminal/transcript";
import "nanocodex-terminal/styles.css";
import "./AgentTerminal.css";
import "./Home.css";
import "./ThreadSharing.css";

type SharedMetadata = { agent_id?: string; title?: string; permission: "read" | "write"; latest_event_cursor?: string };
type SharedEvent = { cursor: string; type: "turn_accepted" | "turn_completed"; turn_id?: string | null; id?: string; input?: string; final_message?: string };
type LiveDelta = { cursor: string; turn_id: string | null; text: string };
type Comment = { id: string; input: string; createdAt?: string | number; created_at?: string | number };
const revokedMessage = "This link is invalid or has been revoked.";
const after = (a: string, b: string) => BigInt(a) > BigInt(b);
const byCursor = (a: { cursor: string }, b: { cursor: string }) => BigInt(a.cursor) < BigInt(b.cursor) ? -1 : BigInt(a.cursor) > BigInt(b.cursor) ? 1 : 0;

// Guest access is intentionally isolated from account cookies, owner sessions and the agent SDK.
// The fragment bearer never enters a URL request, browser storage, telemetry or a tool call.
export function SharedThreadView({ agentId }: { agentId: string }) {
  const [token] = useState(() => {
    const value = new URLSearchParams(window.location.hash.slice(1)).get("token") ?? "";
    return /^nsl_[A-Za-z0-9_-]{43}$/.test(value) ? value : "";
  });
  const base = `/v1/shared/${encodeURIComponent(agentId)}`;
  const [meta, setMeta] = useState<SharedMetadata | null>(null);
  const [events, setEvents] = useState<SharedEvent[]>([]);
  const [live, setLive] = useState<LiveDelta[]>([]);
  const [comments, setComments] = useState<Comment[]>([]);
  const [olderCursor, setOlderCursor] = useState<string | null>(null);
  const [olderPending, setOlderPending] = useState(false);
  const historyExhausted = useRef(false);
  const [olderCommentsCursor, setOlderCommentsCursor] = useState<string | null>(null);
  const [olderCommentsPending, setOlderCommentsPending] = useState(false);
  const commentsExhausted = useRef(false);
  const streamCursor = useRef<string | null>(null);
  const [draft, setDraft] = useState("");
  const [pending, setPending] = useState(false);
  const pendingComment = useRef<{ id: string; input: string } | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const read = useCallback(async (path: string, signal?: AbortSignal) => {
    const response = await fetch(base + path, { headers: { Authorization: `Bearer ${token}` }, credentials: "omit", cache: "no-store", signal });
    if (!response.ok) throw new Error(response.status === 403 || response.status === 404 ? revokedMessage : "The shared thread is unavailable. Try again.");
    return response.json() as Promise<unknown>;
  }, [base, token]);
  const invalidate = useCallback((message = revokedMessage) => {
    setMeta(null); setEvents([]); setLive([]); setComments([]); setOlderCursor(null); setOlderCommentsCursor(null); setError(message);
  }, []);
  const refresh = useCallback(async (signal?: AbortSignal) => {
    if (!token) { invalidate("This link is missing its access token."); setLoading(false); return; }
    setLoading(true); setError("");
    try {
      const metadata = await read("", signal) as SharedMetadata;
      const [history, annotations] = await Promise.all([read("/events/history", signal), read("/comments", signal)]) as [
        { data: SharedEvent[]; has_more: boolean; next_cursor?: string },
        { data: Comment[]; has_more?: boolean; next_cursor?: string },
      ];
      if (signal?.aborted) return;
      streamCursor.current ??= metadata.latest_event_cursor ?? "0";
      setMeta(metadata);
      setEvents((previous) => mergeEvents(previous, Array.isArray(history.data) ? history.data : []));
      setOlderCursor((current) => current ?? (!historyExhausted.current && history.has_more ? history.next_cursor ?? null : null));
      setComments((previous) => mergeComments(previous, annotations.data ?? []));
      setOlderCommentsCursor((current) => current ?? (!commentsExhausted.current && annotations.has_more ? annotations.next_cursor ?? null : null));
    } catch (cause) {
      if (!signal?.aborted) {
        if (cause instanceof Error && cause.message === revokedMessage) invalidate();
        else setError(cause instanceof Error ? cause.message : "Couldn’t open this thread.");
      }
    } finally { if (!signal?.aborted) setLoading(false); }
  }, [read, token, invalidate]);
  useEffect(() => {
    const controller = new AbortController();
    void refresh(controller.signal);
    const timer = window.setInterval(() => { if (document.visibilityState === "visible") void refresh(controller.signal); }, 15_000);
    return () => { controller.abort(); window.clearInterval(timer); };
  }, [refresh]);

  // Authorization headers cannot be supplied to EventSource. Consume a fetch SSE
  // response instead, preserving the server cursor across reconnects and tab sleep.
  useEffect(() => {
    if (!meta || !token) return;
    const controller = new AbortController();
    let active = true;
    const connect = async () => {
      let delay = 500;
      while (active && !controller.signal.aborted) {
        try {
          const cursor = streamCursor.current ?? meta.latest_event_cursor ?? "0";
          const response = await fetch(`${base}/events?after=${encodeURIComponent(cursor)}`, {
            headers: { Authorization: `Bearer ${token}`, accept: "text/event-stream" },
            credentials: "omit", cache: "no-store", signal: controller.signal,
          });
          if (response.status === 403 || response.status === 404) { invalidate(); return; }
          if (!response.ok || !response.body) throw new Error("The live feed is temporarily unavailable.");
          delay = 500;
          const reader = response.body.getReader();
          const decoder = new TextDecoder();
          let buffer = "";
          while (active) {
            const chunk = await reader.read();
            if (chunk.done) break;
            buffer += decoder.decode(chunk.value, { stream: true });
            // Keep a malformed or unbounded SSE frame from accumulating forever.
            if (buffer.length > 1_000_000) throw new Error("The live feed frame was too large.");
            let boundary: number;
            while ((boundary = buffer.indexOf("\n\n")) !== -1) {
              const frame = buffer.slice(0, boundary).replaceAll("\r", "");
              buffer = buffer.slice(boundary + 2);
              const data = frame.split("\n").filter((line) => line.startsWith("data: ")).map((line) => line.slice(6)).join("\n");
              if (!data) continue;
              const event = JSON.parse(data) as SharedEvent | { cursor: string; type: "assistant_delta"; turn_id: string | null; delta: string };
              if (typeof event.cursor !== "string" || !/^\d+$/.test(event.cursor) || !after(event.cursor, streamCursor.current ?? "0")) continue;
              streamCursor.current = event.cursor;
              if (event.type === "assistant_delta") {
                if (typeof event.delta !== "string") continue;
                setLive((current) => {
                  const prior = current.find((row) => row.turn_id === event.turn_id);
                  return prior ? current.map((row) => row === prior ? { ...row, cursor: event.cursor, text: row.text + event.delta } : row)
                    : [...current, { cursor: event.cursor, turn_id: event.turn_id, text: event.delta }];
                });
              } else if (event.type === "turn_accepted" || event.type === "turn_completed") {
                setEvents((current) => mergeEvents(current, [event]));
                if (event.type === "turn_completed") setLive((current) => current.filter((row) => row.turn_id !== (event.turn_id ?? event.id)));
              }
            }
          }
          if (active && !controller.signal.aborted) {
            // A revoked link actively closes its stream. Recheck before retrying,
            // rather than leaving the last private transcript visible indefinitely.
            await read("", controller.signal);
          }
        } catch (cause) {
          if (!active || controller.signal.aborted) return;
          if (cause instanceof Error && cause.message === revokedMessage) { invalidate(); return; }
          await new Promise((resolve) => window.setTimeout(resolve, delay));
          delay = Math.min(delay * 2, 10_000);
        }
      }
    };
    void connect();
    return () => { active = false; controller.abort(); };
  }, [base, token, Boolean(meta), read, invalidate]); // eslint-disable-line react-hooks/exhaustive-deps

  function showGuestError(cause: unknown, fallback: string) {
    const message = cause instanceof Error ? cause.message : fallback;
    if (message === revokedMessage || message === "Comment access is no longer available.") invalidate(message);
    else setError(message);
  }
  async function loadOlder(): Promise<boolean> {
    if (!olderCursor || olderPending) return false;
    setOlderPending(true); setError("");
    try {
      const page = await read(`/events/history?before=${encodeURIComponent(olderCursor)}`) as { data: SharedEvent[]; has_more: boolean; next_cursor?: string };
      setEvents((current) => mergeEvents(current, page.data));
      if (!page.has_more) historyExhausted.current = true;
      setOlderCursor(page.has_more ? page.next_cursor ?? null : null);
      return true;
    } catch (cause) { showGuestError(cause, "Couldn’t load earlier messages."); return false; }
    finally { setOlderPending(false); }
  }
  async function loadOlderComments() {
    if (!olderCommentsCursor || olderCommentsPending) return;
    setOlderCommentsPending(true); setError("");
    try {
      const page = await read(`/comments?before=${encodeURIComponent(olderCommentsCursor)}`) as { data: Comment[]; has_more: boolean; next_cursor?: string };
      setComments((current) => mergeComments(current, page.data));
      if (!page.has_more) commentsExhausted.current = true;
      setOlderCommentsCursor(page.has_more ? page.next_cursor ?? null : null);
    } catch (cause) { showGuestError(cause, "Couldn’t load earlier comments."); }
    finally { setOlderCommentsPending(false); }
  }
  async function submit(value: string) {
    const input = value.trim();
    if (!input || pending || meta?.permission !== "write") return;
    // Preserve the exact intended write and ID after an uncertain response.
    const candidate = pendingComment.current?.input === input ? pendingComment.current : { id: crypto.randomUUID(), input };
    pendingComment.current = candidate;
    setPending(true); setError("");
    const accept = (comment: Comment) => {
      setComments((current) => mergeComments(current, [comment]));
      pendingComment.current = null;
      setDraft("");
    };
    try {
      const response = await fetch(`${base}/comments`, { method: "POST", credentials: "omit", headers: { Authorization: `Bearer ${token}`, "content-type": "application/json" }, body: JSON.stringify(candidate) });
      if (!response.ok) throw new Error(response.status === 403 || response.status === 404 ? "Comment access is no longer available." : "Couldn’t confirm your comment. Try again.");
      accept(await response.json() as Comment);
    } catch (cause) {
      try {
        const page = await read("/comments") as { data: Comment[] };
        const confirmed = page.data.find((item) => item.id === candidate.id && item.input === candidate.input);
        if (confirmed) { accept(confirmed); return; }
      } catch { /* The original error is more useful than a second read error. */ }
      showGuestError(cause, "Couldn’t confirm your comment. Try again.");
    } finally { setPending(false); }
  }
  const transcript = useMemo((): AgentEntry[] => {
    const completed = new Set(events.filter((event) => event.type === "turn_completed").map((event) => event.turn_id ?? event.id));
    const rows: Array<AgentEntry & { cursor: string }> = [];
    for (const event of events) {
      if (event.type === "turn_accepted" && typeof event.input === "string")
        rows.push({ id: `user-${event.cursor}`, cursor: event.cursor, kind: "user", text: event.input });
      if (event.type === "turn_completed" && typeof event.final_message === "string")
        rows.push({ id: `answer-${event.cursor}`, cursor: event.cursor, kind: "assistant", text: event.final_message, streaming: false });
    }
    for (const row of live) if (!completed.has(row.turn_id ?? undefined) && row.text)
      rows.push({ id: `live-${row.turn_id ?? "latest"}`, cursor: row.cursor, kind: "assistant", text: row.text, streaming: true });
    return rows.sort(byCursor).map(({ cursor: _cursor, ...entry }) => entry);
  }, [events, live]);
  const commentComposer = meta?.permission === "write"
    ? <div className="shared-chat-dock">
      <details className="shared-chat-comments"><summary><MessageCircle aria-hidden="true" /> Comments ({comments.length})</summary>
        <div className="shared-chat-comments-body" aria-label="Comments">
          {olderCommentsCursor ? <button type="button" className="shared-thread-older" disabled={olderCommentsPending} onClick={() => { void loadOlderComments(); }}>{olderCommentsPending ? "Loading…" : "Load earlier comments"}</button> : null}
          {comments.length ? comments.map((comment) => <p key={comment.id}><strong>Guest comment</strong><span>{comment.input}</span></p>) : <p>No comments yet.</p>}
        </div>
      </details>
      <TerminalComposer formLabel="Guest comment composer" inputLabel="Comment on this thread" sendLabel="Post comment"
        draft={draft} onChange={setDraft} onSubmit={(value) => { void submit(value); }}
        onCancel={() => {}} pending={pending} running={false} status="ready" placeholder="Leave a comment for the thread owner…" />
      <span className="shared-chat-comment-note">Comments won’t be sent to the AI.</span>
    </div>
    : <div className="shared-chat-dock"><details className="shared-chat-comments"><summary><MessageCircle aria-hidden="true" /> Comments ({comments.length})</summary>
      <div className="shared-chat-comments-body" aria-label="Comments">{comments.map((comment) => <p key={comment.id}><strong>Guest comment</strong><span>{comment.input}</span></p>)}</div>
    </details><p className="shared-thread-readonly"><LockKeyhole aria-hidden="true" /> This link is view only.</p></div>;

  return <main className="nanocodex-demo chat-workspace is-full shared-chat-workspace">
    <div className="conversation-workspace">
      <aside className="shared-chat-sidebar" aria-label="Shared conversation"><a href="/" className="shared-thread-brand"><span className="paradigm-mark" aria-hidden="true" /> Nanocodex</a>
        <div className="shared-chat-sidebar-thread"><MessageCircle aria-hidden="true" /><span>{meta?.title || "Shared thread"}</span></div>
        <p><LockKeyhole aria-hidden="true" /> {meta?.permission === "write" ? "Comments enabled" : "View only"}</p>
      </aside>
      <div className="conversation-main">
        <header className="agent-chat-header"><a href="/" aria-label="Nanocodex home" className="shared-chat-back"><ArrowLeft aria-hidden="true" /></a>
          <div className="agent-chat-heading"><strong>{meta?.title || "Shared thread"}</strong><span>Shared conversation · {meta?.permission === "write" ? "comments enabled" : "view only"}</span></div>
          <div className="agent-chat-header-actions">
            {olderCursor ? <button type="button" className="shared-thread-older" disabled={olderPending} aria-label="Load earlier messages" onClick={() => { void loadOlder(); }}><span className="shared-chat-older-wide">Load earlier messages</span><span className="shared-chat-older-short">Earlier</span></button> : null}
            <button type="button" className="chat-icon-button" onClick={() => { void refresh(); }} disabled={loading} aria-label="Refresh shared thread" title="Refresh shared thread"><RefreshCw aria-hidden="true" /></button>
          </div>
        </header>
        {loading && !meta ? <p role="status" className="shared-thread-state">Opening shared thread…</p> : null}
        {error && !meta ? <div role="alert" className="shared-thread-state"><h1>Can’t open this thread</h1><p>{error}</p><button type="button" onClick={() => { void refresh(); }}>Try again</button></div> : null}
        {meta ? <><div className="shared-chat-boundary"><LockKeyhole aria-hidden="true" /> A view of this conversation. Comments are visible to the owner but never start an AI turn.</div>
          {error ? <p className="shared-thread-error" role="alert">{error}</p> : null}
          <div className="agent-terminal-workspace"><TerminalTranscriptSurface entries={transcript} composer={commentComposer}
            canLoadOlder={Boolean(olderCursor)} isLoadingOlder={olderPending} mode="full" status="ready" inactiveMessage=""
            welcome={loading ? "Opening shared thread…" : "No messages have been shared yet."} onLoadOlder={loadOlder} /></div>
        </> : null}
      </div>
    </div>
  </main>;
}

function mergeEvents(previous: SharedEvent[], incoming: SharedEvent[]) {
  const unique = new Map(previous.map((item) => [item.cursor, item]));
  for (const event of incoming) if (event && typeof event.cursor === "string" && /^\d+$/.test(event.cursor)
    && (event.type === "turn_accepted" || event.type === "turn_completed")) unique.set(event.cursor, event);
  return [...unique.values()].sort(byCursor);
}
function mergeComments(previous: Comment[], incoming: Comment[]) {
  const unique = new Map(previous.map((item) => [item.id, item]));
  for (const item of incoming) if (item?.id) unique.set(item.id, item);
  return [...unique.values()].sort((a, b) => Number(a.created_at ?? a.createdAt ?? 0) - Number(b.created_at ?? b.createdAt ?? 0));
}
