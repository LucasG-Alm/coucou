// Entry point: boot the bridge, wire the island, start the greeting.

import "./style.css";
import { Bridge, IS_TAURI, onEvent, type Placement } from "./core/bridge";
import { Sound } from "./core/sound";
import { State, type Settings } from "./core/state";
import { Island } from "./island/island";
import { registerHookHandlers } from "./island/hooks";
import { registerIntegrationHandlers, refreshConfigured } from "./island/integrations";

async function main() {
  const root = document.getElementById("root");
  if (!root) return;

  void Sound.preload();

  const island = new Island(root);

  const boot = await Bridge.boot();
  if (boot) {
    State.settings = { ...State.settings, ...boot.settings };
  }
  island.applySettings();
  State.loadIntegrationTasks();

  // Where the island lives: dragged there last time, or the notch spot. Listen
  // first, then ask, so a placement sent in between is not lost.
  await onEvent<Placement>("placement", (p) => island.setPlacement(p));
  const placement = await Bridge.placement();
  if (placement) island.setPlacement(placement);

  await onEvent<{ x: number; y: number }>("cursor", ({ x, y }) => island.onCursor(x, y));

  /** Pause has to reach Rust too, or the pollers keep calling out. */
  const setPaused = (on: boolean) => {
    if (State.paused === on) return;
    State.paused = on;
    void Bridge.setPaused(on);
  };

  await onEvent<string>("tray", (what) => {
    switch (what) {
      case "settings":
        setPaused(false);
        island.alert("settings");
        break;
      case "open":
        setPaused(false);
        island.alert(State.defaultView());
        break;
      case "pause":
        setPaused(!State.paused);
        if (State.paused) island.fsm.forceHidden();
        else island.reveal();
        break;
    }
  });

  await onEvent<null>("screen-changed", () => void Bridge.reposition());

  // The settings window writes preferences; apply them here without a restart.
  await onEvent<Settings>("settings-changed", (s) => {
    State.settings = { ...State.settings, ...s };
    island.applySettings();
    State.loadIntegrationTasks();
    void refreshConfigured();
  });

  registerHookHandlers(island);
  registerIntegrationHandlers(island);

  // A terminal closed without saying goodbye (no SessionEnd) would leave its pill
  // behind for good: every half minute, drop the sessions whose window is gone and
  // the ones that have been idle for hours.
  window.setInterval(async () => {
    State.pruneIdleSessions();
    const terminals = State.sessionTerminals;
    if (!terminals.length) return;
    const alive = await Bridge.terminalsAlive(terminals.map((t) => [t.hwnd, t.pid]));
    alive?.forEach((ok, i) => {
      if (!ok) State.endSession(terminals[i].id);
    });
  }, 30_000);

  island.launch();

  // In a plain browser there is no wake strip behind the cursor: make the whole
  // page wake the island so the visuals can be checked with `npm run dev`.
  if (!IS_TAURI) {
    document.addEventListener("click", () => Sound.resume(), { once: true });
  }
}

void main();
