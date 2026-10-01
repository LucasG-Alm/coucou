// "Open terminal": go to the window the session is actually running in.

import { Bridge } from "./bridge";
import type { AgentTask } from "./state";

/**
 * Brings the session's own terminal window to the front. When that window is not
 * known (the relay found none) or is gone, falls back to opening the session's
 * folder in the editor, which is all the old button ever did.
 */
export async function openTaskTerminal(task: AgentTask | null): Promise<void> {
  if (task?.terminalHwnd && task.terminalPid) {
    if (await Bridge.focusTerminal(task.terminalHwnd, task.terminalPid)) return;
  }
  void Bridge.openInVSCode(task?.sessionCwd ?? null);
}
