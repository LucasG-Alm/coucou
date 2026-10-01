// Island geometry — ported from IslandTypes.swift + IslandWindowController.islandSize
// + IslandRootView.botPosition. All values are logical pixels, identical to the
// macOS app's points.

export type IslandMode = "hidden" | "compact" | "expanded";

/**
 * Where the island lives: the notch spot, a vertical strip on the left or right
 * edge, or a horizontal pill left wherever it was dropped (see Settings::dock).
 */
export type DockKind = "top" | "left" | "right" | "free";

/** The compact and hidden forms stand upright on a side edge. */
export const isVertical = (dock: DockKind): boolean => dock === "left" || dock === "right";

export type IslandViewName =
  | "overview"
  | "empty"
  | "approval"
  | "question"
  | "error"
  | "finished"
  | "confused"
  | "upload"
  | "uploading"
  | "choose"
  | "mail"
  | "prompt"
  | "searching"
  | "result"
  | "note"
  | "settings"
  | "greeting";

export type BotStateName =
  | "idle"
  | "working"
  | "thinking"
  | "searching"
  | "approval"
  | "question"
  | "error"
  | "finished"
  | "ratelimit"
  | "sleeping"
  | "dizzy";

export type BotEmoteName = "love" | "surprised" | "proud" | "wink" | "yawn" | "happy" | "annoyed";

export type AgentLayoutMode = "none" | "grid" | "pills" | "column";

export interface ViewLayout {
  height: number;
  botX: number;
  botY: number | null; // null = auto-centred
  botDiameter: number;
  agentMode: AgentLayoutMode;
}

// The window is a fixed 720×320 (largest view) like the macOS panel; the island is
// drawn inside it, glued to the top edge and horizontally centred.
export const PANEL_W = 720;
export const PANEL_H = 320;

// No notch on a PC: these are the hidden/compact sizes from docs/SPEC.md.
export const NOTCH_W = 184;
export const NOTCH_H = 32;
export const COMPACT_W = 288; // NOTCH_W + 104

// Compact island standing on a side edge: Mochi on top and the other agents'
// mini Mochis stacked in a single column under it, so its height follows how many
// there are instead of the fixed 288 px of the lying-down pill.
export const COMPACT_MINI = 16;
export const COMPACT_MINI_GAP = 4;
export const COMPACT_COLUMN_TOP = 36;
export const COMPACT_COLUMN_MAX = 6;

/** Height of the compact column for this many other agents (0 = just Mochi). */
export function compactColumnHeight(pills: number): number {
  const n = Math.min(COMPACT_COLUMN_MAX, Math.max(0, pills));
  if (n === 0) return COMPACT_COLUMN_TOP;
  return COMPACT_COLUMN_TOP + n * COMPACT_MINI + (n - 1) * COMPACT_MINI_GAP + 10;
}
export const EXPANDED_W = 640;

export const ROUNDED_CORNER = 14; // hidden / compact
export const EXPANDED_CORNER = 22;

/** Invisible hover strip that wakes the island when hidden. */
export const WAKE_STRIP_W = 240;
export const WAKE_STRIP_H = 6;

export const VIEW_LAYOUTS: Record<IslandViewName, ViewLayout> = {
  overview: { height: 160, botX: 68, botY: null, botDiameter: 58, agentMode: "pills" },
  empty: { height: 160, botX: 70, botY: null, botDiameter: 62, agentMode: "none" },
  approval: { height: 160, botX: 62, botY: null, botDiameter: 56, agentMode: "column" },
  question: { height: 160, botX: 62, botY: null, botDiameter: 56, agentMode: "column" },
  error: { height: 160, botX: 62, botY: null, botDiameter: 58, agentMode: "column" },
  finished: { height: 160, botX: 62, botY: null, botDiameter: 58, agentMode: "column" },
  confused: { height: 160, botX: 76, botY: null, botDiameter: 66, agentMode: "column" },
  upload: { height: 176, botX: 140, botY: 104, botDiameter: 62, agentMode: "column" },
  // botY 103 = bar top (42 + 58) + 3, so the dot really rides the bar. The Swift
  // layout says 118 while its own comment says 103; the comment matches the spec.
  uploading: { height: 176, botX: 46, botY: 103, botDiameter: 20, agentMode: "none" },
  choose: { height: 176, botX: 60, botY: 101, botDiameter: 52, agentMode: "column" },
  mail: { height: 240, botX: 56, botY: null, botDiameter: 46, agentMode: "column" },
  prompt: { height: 160, botX: 52, botY: null, botDiameter: 44, agentMode: "column" },
  searching: { height: 160, botX: 52, botY: null, botDiameter: 44, agentMode: "column" },
  result: { height: 160, botX: 52, botY: null, botDiameter: 44, agentMode: "column" },
  note: { height: 160, botX: 60, botY: null, botDiameter: 50, agentMode: "column" },
  settings: { height: 160, botX: 54, botY: null, botDiameter: 46, agentMode: "none" },
  greeting: { height: 150, botX: 320, botY: 90, botDiameter: 0, agentMode: "none" },
};

// The upload views above are only the fallback geometry. Once a file is actually
// dropped the whole sequence — Mochi included — is drawn by src/upload, which
// owns its own constants (USC) straight from UploadSequenceEngine.swift.

/** Chat view grows with the conversation — IslandContainer.chatPromptHeight. */
export function chatPromptHeight(messageCount: number): number {
  return Math.min(300, 240 + messageCount * 40);
}

// ── Open island on a side edge ──────────────────────────────────────────────────
// Docked left or right, the open island is a narrow column instead of the wide
// panel. Most views are a single full-width card, so they reflow on their own; the
// overview is the exception (card and agent pills side by side), and the CSS class
// `upright` stacks them.

/** 322 px for the overview's left card, plus the 10 px of padding on each side. */
export const UPRIGHT_W = 342;

/**
 * Views drawn on a fixed 640 px canvas — the launch greeting and the drop
 * sequence — can't reflow, so even on a side edge they open as the wide panel.
 */
const WIDE_ONLY: ReadonlySet<IslandViewName> = new Set(["greeting", "upload", "uploading", "choose"]);

/** Whether this view opens as the narrow column when docked here. */
export const usesUprightLayout = (dock: DockKind, view: IslandViewName): boolean =>
  isVertical(dock) && !WIDE_ONLY.has(view);

/**
 * Island heights for the narrow layout: 8 px top padding + 34 px header + the card
 * + 10 px bottom padding. Text wraps in 190 px instead of 380, so these are taller
 * than the wide ones. None may exceed PANEL_H, the window's height.
 */
const UPRIGHT_HEIGHTS: Record<IslandViewName, number> = {
  overview: 278, // the tallest: three rows of pills; see uprightOverviewHeight()
  empty: 190,
  approval: 220,
  question: 190,
  error: 220,
  finished: 190,
  confused: 160,
  note: 160,
  settings: 230,
  mail: 160,
  searching: 160,
  result: 160,
  prompt: 300, // replaced by uprightChatHeight()
  // Never used here (see WIDE_ONLY); listed so the record stays complete.
  greeting: 150,
  upload: 176,
  uploading: 176,
  choose: 176,
};

function uprightChatHeight(messageCount: number): number {
  return Math.min(PANEL_H, 280 + messageCount * 40);
}

/**
 * The overview stacks its 108 px card over the agent pills, two to a row and at
 * most three rows, so its height follows how many pills there are: 8 top padding +
 * 34 header + 108 card + 10 gap + the pills' card + 10 bottom padding.
 */
function uprightOverviewHeight(pills: number): number {
  const rows = Math.min(3, Math.max(1, Math.ceil(pills / 2)));
  return 8 + 34 + 108 + 10 + (rows * 28 + (rows - 1) * 4 + 16) + 10;
}

export function islandSize(
  mode: IslandMode,
  view: IslandViewName,
  chatCount = 0,
  dock: DockKind = "top",
  /** How many agent pills sit beside / under the overview card. */
  pills = 0,
): { w: number; h: number } {
  const upright = isVertical(dock);
  switch (mode) {
    case "hidden":
      // No notch to hide inside on a PC: the island retracts to zero height and
      // slides into the top edge of the screen instead of sitting there as a bar.
      // On a side edge it retracts to zero width instead.
      return upright ? { w: 0, h: NOTCH_W } : { w: NOTCH_W, h: 0 };
    case "compact":
      // Stood on its end it is 32 wide and only as tall as its agents need.
      return upright ? { w: NOTCH_H, h: compactColumnHeight(pills) } : { w: COMPACT_W, h: NOTCH_H };
    case "expanded": {
      if (usesUprightLayout(dock, view)) {
        const h =
          view === "prompt" ? uprightChatHeight(chatCount)
          : view === "overview" ? uprightOverviewHeight(pills)
          : UPRIGHT_HEIGHTS[view];
        return { w: UPRIGHT_W, h };
      }
      const h = view === "prompt" ? chatPromptHeight(chatCount) : VIEW_LAYOUTS[view].height;
      return { w: EXPANDED_W, h };
    }
  }
}

export interface BotPlacement {
  cx: number;
  cy: number;
  diameter: number;
  opacity: number;
}

/** IslandRootView.botPosition — cy is measured from the island's top edge. */
export function botPosition(
  mode: IslandMode,
  view: IslandViewName,
  islandH: number,
  uploadProgress = 0,
  dock: DockKind = "top",
): BotPlacement {
  const upright = isVertical(dock);
  switch (mode) {
    case "hidden":
      return upright
        ? { cx: NOTCH_H / 2, cy: 46, diameter: 6, opacity: 0 }
        : { cx: 46, cy: 16, diameter: 6, opacity: 0 };
    case "compact":
      // Standing on its end Mochi sits at the top, centred across the 32 px width,
      // with the mini Mochis stacked right under it (see COMPACT_COLUMN_TOP).
      return upright
        ? { cx: NOTCH_H / 2, cy: 18, diameter: 20, opacity: 1 }
        : { cx: 40, cy: 16, diameter: 20, opacity: 1 };
    case "expanded": {
      const layout = VIEW_LAYOUTS[view];
      if (usesUprightLayout(dock, view)) {
        // Mochi stays at the left of its card, as in the wide layout. In the
        // overview that card is the top one (the pills drop below it), so its
        // centre is fixed; every other view is one card spanning the whole height.
        const cy = view === "overview" ? 42 + 108 / 2 : 42 + (islandH - 52) / 2;
        return { cx: layout.botX, cy, diameter: layout.botDiameter, opacity: 1 };
      }
      if (view === "uploading") {
        return {
          cx: 36 + uploadProgress * 526,
          cy: layout.botY ?? 103,
          diameter: layout.botDiameter,
          opacity: 1,
        };
      }
      if (layout.botY != null) {
        return { cx: layout.botX, cy: layout.botY, diameter: layout.botDiameter, opacity: 1 };
      }
      // Centre of the fixed 84 pt card (8 pt top inset + 34 pt header → content at y = 42)
      const headerBottom = 42;
      const cardH = 84;
      const cy = headerBottom + (islandH - headerBottom - cardH) / 2 + cardH / 2;
      return { cx: layout.botX, cy, diameter: layout.botDiameter, opacity: 1 };
    }
  }
}

export function botGlowColor(s: BotStateName): string {
  switch (s) {
    case "working":
      return "#3B9EFF";
    case "thinking":
      return "#A78BFA";
    case "searching":
      return "#6366F1";
    case "approval":
      return "#F5A524";
    case "error":
      return "#F4505E";
    case "finished":
      return "#34D399";
    case "ratelimit":
      return "#F59E0B";
    default:
      return "#FFFFFF";
  }
}

export function botGlowOpacity(s: BotStateName): number {
  switch (s) {
    case "idle":
    case "sleeping":
      return 0.15;
    case "dizzy":
      return 0;
    default:
      return 0.65;
  }
}

// Project colours (IslandConst.projectColors)
const PROJECT_COLORS: Record<string, string> = {
  korus: "#FF5A4E",
  "sbe hub": "#2EC4A0",
  "morning ai brief": "#F29B38",
  "publication ig": "#7C5CFF",
  "ig post": "#7C5CFF",
  "louisraille.fr": "#38BDF8",
  louisraille: "#38BDF8",
  "notch buddy": "#EC4899",
  "notch-buddy": "#EC4899",
  notchbuddy: "#EC4899",
};

const FALLBACK_COLORS = ["#22C55E", "#EAB308", "#60A5FA", "#E879F9"];

export function colorForProject(name: string): string {
  const key = name.toLowerCase().trim();
  const exact = PROJECT_COLORS[key];
  if (exact) return exact;
  for (const [k, c] of Object.entries(PROJECT_COLORS)) {
    if (key.startsWith(k) || key.includes(k)) return c;
  }
  let hash = 0;
  for (let i = 0; i < name.length; i++) hash = (hash * 31 + name.charCodeAt(i)) | 0;
  return FALLBACK_COLORS[Math.abs(hash) % FALLBACK_COLORS.length];
}

// Card wash colours (CardBackground.washColor)
export type Wash = "red" | "green" | "pink" | "amber" | "cyan" | "indigo" | "soft" | null;

export function washRGBA(wash: Wash): string {
  switch (wash) {
    case "red":
      return "rgba(244,80,94,0.55)";
    case "green":
      return "rgba(52,211,153,0.5)";
    case "pink":
      return "rgba(244,114,182,0.55)";
    case "amber":
      return "rgba(245,165,36,0.42)";
    case "cyan":
      return "rgba(34,211,238,0.38)";
    case "indigo":
      return "rgba(99,102,241,0.5)";
    case "soft":
      return "rgba(255,255,255,0.08)";
    default:
      return "rgba(0,0,0,0)";
  }
}
