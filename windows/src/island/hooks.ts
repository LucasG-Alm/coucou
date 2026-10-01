// Claude Code and Codex hook events → island state.
// Port of HookServer.processEvent / processPermissionRequest from the macOS app.
// Difference from macOS: no terminal filter. On Windows the hook fires from any
// terminal (Windows Terminal, VS Code, PowerShell…) and all of them are handled.
// Each agent owns one pill; `coucou-hook --agent` says whose event this is.

import { Bridge, onEvent } from "../core/bridge";
import { Sound } from "../core/sound";
import { AGENT_TASK_IDS, defaultTaskName, State } from "../core/state";
import type { Island } from "./island";

/** Clears the approval card if no decision was made before the hook gave up. */
let pendingTimeout: number | null = null;

interface HookPayload {
  hook_event_name?: string;
  /** "claude" (the default) or "codex" — see `coucou-hook --agent`. */
  agent?: string;
  /** The window the session's terminal lives in (found by the relay; absent if unknown). */
  terminal_hwnd?: number;
  terminal_pid?: number;
  terminal_exe?: string;
  request_id?: string;
  session_id?: string;
  cwd?: string;
  /** The folder the agent was started in; unlike `cwd` it does not follow a `cd`. Empty if unknown. */
  project_dir?: string;
  message?: string;
  /** Codex's Stop carries the last answer here instead of `message`. */
  last_assistant_message?: string | null;
  /** UserPromptSubmit carries `prompt`; `message` belongs to Notification/Stop. */
  prompt?: string;
  tool_name?: string;
  tool_input?: Record<string, unknown>;
}

const PROJECT_ALIASES: Record<string, string> = {
  "notch-buddy": "Notch Buddy",
  notchbuddy: "Notch Buddy",
  notch_buddy: "Notch Buddy",
};

function aliasProjectName(name: string): string {
  return PROJECT_ALIASES[name.toLowerCase()] ?? name;
}

function lastPathComponent(p: string): string {
  const cleaned = p.replace(/[\\/]+$/, "");
  const idx = Math.max(cleaned.lastIndexOf("\\"), cleaned.lastIndexOf("/"));
  return idx >= 0 ? cleaned.slice(idx + 1) : cleaned;
}

/** frenchStep() — same labels as the macOS app. */
const TOOL_LABELS: Record<string, string> = {
  Bash: "Exécute",
  Read: "Lit",
  Write: "Écrit",
  Edit: "Modifie",
  Glob: "Cherche",
  Grep: "Recherche",
  WebSearch: "Recherche web",
  WebFetch: "Récupère",
  TodoWrite: "Tâches",
  Task: "Agent",
  LS: "Liste",
  MultiEdit: "Modifie",
  NotebookEdit: "Notebook",
  PowerShell: "Exécute",
  // Codex reports `Bash` for shell and exec_command, and `apply_patch` for edits.
  apply_patch: "Modifie",
};

function stepLabel(tool: string, input: Record<string, unknown>): string {
  const label = TOOL_LABELS[tool] ?? tool;
  const str = (k: string) => (typeof input[k] === "string" ? (input[k] as string) : null);
  const cmd = str("command");
  if (cmd) return `${label} · ${cmd.slice(0, 40)}`;
  const path = str("path");
  if (path) return `${label} · ${lastPathComponent(path)}`;
  const file = str("file_path");
  if (file) return `${label} · ${lastPathComponent(file)}`;
  const query = str("query");
  if (query) return `${label} · ${query.slice(0, 40)}`;
  return label;
}

/**
 * What the Allow button actually authorises. Approving "Write" tells you nothing
 * — approving `Write · C:\…\.env` tells you everything, and the difference is
 * the whole point of approving from the island rather than blind.
 *
 * Ordered by how specific the field is, so an unfamiliar tool still shows
 * whatever identifying string it carries instead of falling back to its name.
 */
const APPROVAL_FIELDS = [
  "command", // Bash, PowerShell
  "file_path", // Write, Edit, MultiEdit, NotebookEdit
  "path", // Read, LS
  "url", // WebFetch
  "query", // WebSearch
  "pattern", // Glob, Grep
  "prompt", // Task
] as const;

function approvalTarget(tool: string, input: Record<string, unknown>): string {
  for (const field of APPROVAL_FIELDS) {
    const value = input[field];
    if (typeof value === "string" && value.trim()) {
      return `${tool} · ${value.trim()}`;
    }
  }
  return tool;
}

function upsert(id: string, projectName: string, cwd: string) {
  const t = State.tasks.find((x) => x.id === id);
  if (!t) return;
  // A session keeps the name it was first given. The working directory wanders
  // inside one run (every `cd`), and a pill whose label changes under the cursor
  // is a pill that is hard to click.
  if (!t.sessionId || !t.sessionCwd) t.name = projectName;
  if (cwd) t.sessionCwd = cwd;
}

function clearSession(id: string) {
  const t = State.tasks.find((x) => x.id === id);
  if (!t) return;
  t.steps = [];
  t.stepIndex = 0;
  t.name = defaultTaskName(id);
  t.pillBadge = null;
}

export function registerHookHandlers(island: Island) {
  void onEvent<HookPayload>("hook", (payload) => handleHook(island, payload));
}

function handleHook(island: Island, payload: HookPayload) {
  if (State.paused) {
    // Silence here used to cost Claude Code nearly two minutes: the relay waited
    // for a decision from an island that had already decided not to look. Say so,
    // and the terminal takes the question immediately.
    if (payload.request_id) void Bridge.approvalDecline(payload.request_id);
    return;
  }

  // An agent we have no pill for (a future one, or a typo) is handed straight
  // back to its terminal rather than left waiting on a card nobody will see.
  const agent = payload.agent ?? "claude";
  const baseId = AGENT_TASK_IDS[agent];
  if (!baseId) {
    if (payload.request_id) void Bridge.approvalDecline(payload.request_id);
    return;
  }
  // One pill per live session, i.e. per terminal: the first event of a session
  // makes it. An event with no session id falls back to the agent's own pill.
  const session = payload.session_id ? State.ensureSession(agent, payload.session_id) : null;
  const id = session?.id ?? baseId;
  const owner = State.tasks.find((t) => t.id === id);
  if (owner) {
    owner.lastEvent = performance.now();
    // The relay only looks the window up on some events; keep the last one it found.
    if (payload.terminal_hwnd) {
      owner.terminalHwnd = payload.terminal_hwnd;
      owner.terminalPid = payload.terminal_pid ?? null;
      owner.terminalExe = payload.terminal_exe ?? null;
    }
  }

  const name = payload.hook_event_name ?? "";
  const cwd = payload.cwd ?? "";
  // Name the session after its project root when the agent told us one.
  const raw = lastPathComponent(payload.project_dir || cwd);
  const projectName = aliasProjectName(raw || "Session");
  const focused = State.focusId === id;

  /** Alerts force the island open; work events only reveal the compact island. */
  const surface = (view: Parameters<Island["alert"]>[0], isAlert: boolean) => {
    if (State.mode === "expanded") {
      if (isAlert) island.setView(view);
    } else if (isAlert) {
      island.alert(view);
    } else if (State.mode === "hidden") {
      island.reveal();
    }
  };

  switch (name) {
    case "SessionStart":
      upsert(id, projectName, cwd);
      surface("overview", false);
      Sound.play("work");
      break;

    case "UserPromptSubmit": {
      upsert(id, projectName, cwd);
      State.updateTask(id, "thinking");
      // The field is `prompt`; reading `message` meant this step was always blank.
      const asked = payload.prompt ?? payload.message;
      if (asked) State.appendStep(id, asked.slice(0, 60));
      surface("overview", false);
      break;
    }

    case "PreToolUse": {
      upsert(id, projectName, cwd);
      State.updateTask(id, "working");
      const tool = payload.tool_name ?? "Tool";
      State.appendStep(id, stepLabel(tool, payload.tool_input ?? {}));
      surface("overview", false);
      break;
    }

    case "PostToolUse":
      State.updateTask(id, "working");
      break;

    case "PostToolUseFailure":
      State.updateTask(id, "working");
      State.appendStep(id, "⚠ failed");
      break;

    case "Notification": {
      const message = payload.message ?? "";
      const lower = message.toLowerCase();
      if (lower.includes("rate limit") || lower.includes("limite d")) {
        State.updateTask(id, "ratelimit");
        Sound.play("rate");
      } else if (message.endsWith("?")) {
        State.updateTask(id, "question");
        State.appendStep(id, message);
      }
      break;
    }

    case "Stop":
      State.updateTask(id, "finished");
      {
        const last = payload.message ?? payload.last_assistant_message;
        if (last) State.appendStep(id, last.slice(0, 60));
      }
      Sound.play("finish");
      if (focused) surface("finished", true);
      else State.setPillBadge(id, "finished");
      window.setTimeout(() => {
        State.updateTask(id, "idle");
        State.setPillBadge(id, null);
      }, 5200);
      break;

    case "StopFailure":
      State.updateTask(id, "error");
      Sound.play("error");
      if (focused) surface("error", true);
      else State.setPillBadge(id, "error");
      break;

    case "SessionEnd":
      // A session's pill goes with it; an agent's own pill just goes quiet.
      if (session) {
        State.endSession(id);
      } else {
        State.updateTask(id, "idle");
        clearSession(id);
      }
      break;

    // Codex: the turn was interrupted from the terminal.
    case "Interrupt":
      State.updateTask(id, "idle");
      break;

    case "SubagentStart":
      State.appendStep(id, "+ subagent");
      break;

    case "SubagentStop":
      State.appendStep(id, "• subagent done");
      break;

    case "PermissionRequest": {
      const requestId = payload.request_id ?? "";
      // One card, one request. A second one must never quietly replace the first
      // — that would leave a human staring at request B while request A waits for
      // a decision nobody can give. Hand it straight back to the terminal.
      if (State.pendingApproval && State.pendingApproval.requestId !== requestId) {
        if (requestId) void Bridge.approvalDecline(requestId);
        break;
      }
      upsert(id, projectName, cwd);
      if (pendingTimeout != null) window.clearTimeout(pendingTimeout);
      const tool = payload.tool_name ?? "Tool";
      const input = payload.tool_input ?? {};
      State.pendingApproval = {
        requestId,
        taskId: id,
        sessionId: payload.session_id ?? "",
        tool,
        command: approvalTarget(tool, input),
      };
      // The relay's short ack window closes in 800 ms; everything below this
      // line is synchronous, so the card really is up by the time it lands.
      if (requestId) void Bridge.approvalAck(requestId);
      State.updateTask(id, "approval");
      State.isPinned = true;
      Sound.play("approval");
      // An agent is blocked on a human, so its card always comes up, even when
      // another agent holds the view. Upstream only badged the pill in that case,
      // but nothing opens the card from a focused pill, so a second agent's
      // request could never be answered. We just told the relay a human can act.
      if (!focused) State.setFocus(id);
      island.alert("approval");
      // Coucou answers within 108 s or not at all; after that the terminal has
      // taken over and the card would be lying.
      pendingTimeout = window.setTimeout(() => {
        pendingTimeout = null;
        if (!State.pendingApproval) return;
        State.pendingApproval = null;
        State.isPinned = false;
        island.dropPin();
        State.updateTask(id, "working");
        State.setPillBadge(id, null);
        if (State.view === "approval") island.setView(State.defaultView());
        State.notify();
      }, 110_000);
      break;
    }

    default:
      break;
  }
  State.notify();
}
