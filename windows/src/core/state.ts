// App state — mirror of AppState.swift (the parts the island needs).

import type { BotEmoteName, BotStateName, DockKind, IslandMode, IslandViewName } from "./layout";
import type { EyeShape } from "../mochi/engine";

export type AgentSource = "claudeCode" | "codex" | "n8n" | "agent";
export type PillBadge = "approval" | "finished" | "error";

export interface AgentTask {
  id: string;
  name: string;
  color: string;
  state: BotStateName;
  stepIndex: number;
  steps: string[];
  source: AgentSource;
  isIntegration: boolean;
  emote?: BotEmoteName | null;
  miniEye?: EyeShape | null;
  pillBadge?: PillBadge | null;
  sessionCwd?: string | null;
  /**
   * Set on a task that stands for one live session — one terminal, one agent run —
   * rather than for the agent itself. Its `id` is `<agent task id>#<session id>`.
   */
  sessionId?: string;
  /** The window that session's terminal lives in, as found by the relay. */
  terminalHwnd?: number | null;
  terminalPid?: number | null;
  terminalExe?: string | null;
  /** performance.now() of the session's last event, to retire the ones that go quiet. */
  lastEvent?: number;
}

export interface ApprovalInfo {
  requestId: string;
  /** The task (agent pill) that asked, so Allow/Deny updates the right one. */
  taskId: string;
  sessionId: string;
  tool: string;
  command: string;
}

export interface ChatMessage {
  id: number;
  role: "user" | "assistant";
  content: string;
}

export type PromptContext =
  | { kind: "window"; appName: string; title: string; url?: string }
  | { kind: "file"; name: string; path?: string };

export interface ResultItem {
  label: string;
  detail: string;
  url?: string;
}

export interface SearchResult {
  title: string;
  items: ResultItem[];
  note?: string;
}

const task = (
  id: string, name: string, color: string, source: AgentSource,
): AgentTask => ({
  id, name, color, state: "idle", stepIndex: 0, steps: [], source, isIntegration: true,
});

/**
 * AgentTask.integrationAgents — same ids, names and colours as macOS, except the
 * AI agents: Claude Code is terracotta and Codex a bluish grey, so the Mochi tells
 * them apart at a glance.
 */
export const INTEGRATION_AGENTS: AgentTask[] = [
  task("integration_claude", "Claude Code", "#D97757", "claudeCode"),
  task("integration_codex", "Codex", "#93A0B4", "codex"),
  task("integration_resend", "Resend", "#22C55E", "n8n"),
  task("integration_n8n", "n8n", "#F29B38", "n8n"),
  task("integration_vercel", "Vercel", "#7C5CFF", "n8n"),
  task("integration_github", "GitHub", "#F4505E", "n8n"),
  task("integration_notion", "Notion", "#8C8C8C", "n8n"),
  task("integration_calcom", "Cal.com", "#C9956A", "n8n"),
  task("integration_stripe", "Stripe", "#0570DE", "n8n"),
];

/** `agent` field of a hook payload (set by `coucou-hook --agent`) → its pill. */
export const AGENT_TASK_IDS: Record<string, string> = {
  claude: "integration_claude",
  codex: "integration_codex",
};

/** The AI agents are always on; only the polled integrations are opt-in. */
const ALWAYS_ON_IDS = Object.values(AGENT_TASK_IDS);

export const AGENT_LABEL: Record<AgentSource, string> = {
  claudeCode: "Claude Code",
  codex: "Codex",
  n8n: "n8n",
};

/** A hook-driven AI session, as opposed to a polled integration. */
export const isAgentTask = (t: AgentTask): boolean => t.source !== "n8n";

/** What a pill is called when no session is running. */
export const defaultTaskName = (id: string): string => {
  const base = id.split("#")[0];
  return INTEGRATION_AGENTS.find((t) => t.id === base)?.name ?? id;
};

/**
 * The label of a pill: the agent's name for an agent or an integration, and for a
 * session the folder it runs in — with a number when two sessions of the *same
 * agent* share a folder, so two terminals in one project can still be told apart.
 * Different agents in one folder need no number: their colours already differ.
 */
export function taskLabel(task: AgentTask, all: AgentTask[]): string {
  if (!task.sessionId) return defaultTaskName(task.id);
  const sameName = all.filter(
    (t) => t.sessionId && t.name === task.name && t.source === task.source,
  );
  return sameName.length > 1 ? `${task.name} ·${sameName.indexOf(task) + 1}` : task.name;
}

/** A session retires after this long without an event. */
const SESSION_IDLE_MS = 3 * 60 * 60 * 1000;

export const TOGGLEABLE_INTEGRATION_IDS = [
  "integration_resend", "integration_n8n", "integration_vercel", "integration_github",
  "integration_notion", "integration_calcom", "integration_stripe",
];

/** What an integration poller last reported. */
export interface IntegrationInfo {
  data: Record<string, unknown>;
  error: string | null;
  loaded: boolean;
  configured: boolean;
}

export interface Settings {
  soundEnabled: boolean;
  soundVolume: number;
  autoCloseInterval: number;
  absenceInterval: number;
  activeIntegrations: string[];
  screen: "primary" | "cursor";
  autostart: boolean;
  hooksInstalled: boolean;
  /** Claude model used by the chat. */
  model: string;
  /** Where the island sits. Set by dragging it; Rust owns the stored value. */
  dock: DockKind;
  dockX: number;
  dockY: number;
}

export const DEFAULT_SETTINGS: Settings = {
  soundEnabled: true,
  soundVolume: 0.12,
  autoCloseInterval: 15,
  absenceInterval: 180,
  activeIntegrations: [
    "integration_resend", "integration_n8n", "integration_vercel", "integration_github",
  ],
  screen: "primary",
  autostart: false,
  hooksInstalled: false,
  model: "claude-opus-5",
  dock: "top",
  dockX: 0,
  dockY: 0,
};

type Listener = () => void;

class AppState {
  mode: IslandMode = "hidden";
  view: IslandViewName = "overview";

  tasks: AgentTask[] = [];
  /** One entry per live terminal session; `tasks` shows these in place of the agent's own pill. */
  private sessions: AgentTask[] = [];
  focusId: string | null = null;

  stateOverride: BotStateName | null = null;

  /** Cursor in logical screen pixels, origin top-left (like AppState.mousePosition). */
  mouse = { x: 0, y: 0 };
  /** Cursor relative to the island's top-left corner. */
  mouseInIsland = { x: 0, y: 0 };

  isPinned = false;
  paused = false;

  uploadProgress = 0;
  uploadDuration = 2.4;
  fileDragOver = false;

  promptContext: PromptContext | null = null;
  droppedFile: { name: string; path: string } | null = null;
  noteMessage: string | null = null;
  searchResult: SearchResult | null = null;
  chatHistory: ChatMessage[] = [];
  pendingApproval: ApprovalInfo | null = null;

  integrations: Record<string, IntegrationInfo> = {};

  lastActivity = performance.now();

  settings: Settings = { ...DEFAULT_SETTINGS };

  private listeners = new Set<Listener>();

  subscribe(fn: Listener): () => void {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  }

  /** Marks the UI dirty; the island re-renders on the next frame. */
  notify() {
    for (const fn of this.listeners) fn();
  }

  get focusTask(): AgentTask | null {
    return this.tasks.find((t) => t.id === this.focusId) ?? this.tasks[0] ?? null;
  }

  get effectiveState(): BotStateName {
    return this.stateOverride ?? this.focusTask?.state ?? "idle";
  }

  get otherTasks(): AgentTask[] {
    return this.tasks.filter((t) => t.id !== this.focusId);
  }

  setFocus(id: string) {
    const t = this.tasks.find((x) => x.id === id);
    if (!t) return;
    this.focusId = id;
    t.pillBadge = null;
    this.notify();
  }

  updateTask(id: string, state: BotStateName) {
    const t = this.tasks.find((x) => x.id === id);
    if (!t) return;
    t.state = state;
    this.notify();
  }

  appendStep(id: string, step: string) {
    const t = this.tasks.find((x) => x.id === id);
    if (!t) return;
    t.steps.push(step);
    if (t.steps.length > 20) t.steps.shift();
    t.stepIndex = t.steps.length - 1;
    this.notify();
  }

  setPillBadge(id: string, badge: PillBadge | null) {
    const t = this.tasks.find((x) => x.id === id);
    if (!t) return;
    t.pillBadge = badge;
    this.notify();
  }

  /**
   * loadIntegrationTasks() — the AI agents always on, the rest opt-in (max 4).
   * An agent with live sessions shows one pill per session instead of its own, and
   * its own pill comes back when the last session ends. Existing tasks are reused,
   * so their state survives a rebuild.
   */
  loadIntegrationTasks() {
    const existing = new Map(this.tasks.map((t) => [t.id, t]));
    const next: AgentTask[] = [];
    // The declared order, so pills never shuffle; a session stays where it was made.
    for (const proto of INTEGRATION_AGENTS) {
      if (ALWAYS_ON_IDS.includes(proto.id)) {
        const mine = this.sessions.filter((s) => s.id.startsWith(`${proto.id}#`));
        if (mine.length) next.push(...mine);
        else next.push(existing.get(proto.id) ?? { ...proto, steps: [] });
      } else if (this.settings.activeIntegrations.includes(proto.id)) {
        next.push(existing.get(proto.id) ?? { ...proto, steps: [] });
      }
    }
    // Third-party agent pills (coucou_agent) are not rebuilt here: keep them, right
    // after the Claude pills so they sit inside the visible slice(0,4).
    const external = this.tasks.filter((t) => t.id.startsWith("agent_"));
    if (external.length) {
      let at = next.length;
      while (at > 0 && !next[at - 1].id.startsWith(AGENT_TASK_IDS.claude)) at--;
      next.splice(at, 0, ...external);
    }
    this.tasks = next;
    // Whatever was focused may be gone (a placeholder replaced by its session).
    if (!next.some((t) => t.id === this.focusId)) this.focusId = next[0]?.id ?? null;
    this.notify();
  }

  removeTask(id: string) {
    const idx = this.tasks.findIndex((t) => t.id === id);
    if (idx < 0) return;
    this.tasks.splice(idx, 1);
    if (this.focusId === id) this.focusId = this.tasks[0]?.id ?? null;
    this.notify();
  }

  /** Creates a dynamic agent_ pill on first event; no-ops if it already exists.
   *  Inserted after the Claude pills so it appears in the visible slice(0,4). */
  upsertExternalAgent(id: string, name: string, color: string) {
    if (this.tasks.some((t) => t.id === id)) return;
    let at = this.tasks.length;
    while (at > 0 && !this.tasks[at - 1].id.startsWith(AGENT_TASK_IDS.claude)) at--;
    this.tasks.splice(at, 0, {
      id, name, color,
      state: "idle", stepIndex: 0, steps: [],
      source: "agent", isIntegration: false,
    });
    if (!this.focusId) this.focusId = id;
    this.notify();
  }

  /**
   * The pill for one live session, made on its first event. Null for an agent we
   * have no pill for.
   */
  ensureSession(agent: string, sessionId: string): AgentTask | null {
    const baseId = AGENT_TASK_IDS[agent];
    const proto = INTEGRATION_AGENTS.find((t) => t.id === baseId);
    if (!baseId || !proto) return null;
    const id = `${baseId}#${sessionId}`;
    let session = this.sessions.find((s) => s.id === id);
    if (!session) {
      session = { ...proto, id, steps: [], sessionId, lastEvent: performance.now() };
      this.sessions.push(session);
      // A placeholder that held the focus hands it to the session replacing it.
      const hadFocus = this.focusId === baseId;
      this.loadIntegrationTasks();
      if (hadFocus) this.focusId = id;
    }
    return session;
  }

  /** The session ended (or its terminal is gone): its pill goes away. */
  endSession(id: string) {
    const before = this.sessions.length;
    this.sessions = this.sessions.filter((s) => s.id !== id);
    if (this.sessions.length === before) return;
    if (this.focusId === id) this.focusId = null;
    this.loadIntegrationTasks();
  }

  /** The sessions that carry a terminal window, for the liveness check. */
  get sessionTerminals(): { id: string; hwnd: number; pid: number }[] {
    return this.sessions
      .filter((s) => s.terminalHwnd && s.terminalPid)
      .map((s) => ({ id: s.id, hwnd: s.terminalHwnd!, pid: s.terminalPid! }));
  }

  /** Retires the sessions that have been quiet and idle for hours. */
  pruneIdleSessions(now = performance.now()) {
    for (const s of [...this.sessions]) {
      if (s.state === "idle" && now - (s.lastEvent ?? now) > SESSION_IDLE_MS) this.endSession(s.id);
    }
  }

  toggleIntegration(id: string) {
    if (ALWAYS_ON_IDS.includes(id)) return;
    const active = this.settings.activeIntegrations;
    if (active.includes(id)) {
      this.settings.activeIntegrations = active.filter((x) => x !== id);
      if (this.focusId === id) this.focusId = null; // loadIntegrationTasks picks the first
    } else {
      if (active.length >= 4) return;
      this.settings.activeIntegrations = [...active, id];
    }
    this.loadIntegrationTasks();
  }

  defaultView(): IslandViewName {
    return this.tasks.length === 0 ? "empty" : "overview";
  }
}

export const State = new AppState();
