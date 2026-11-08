import type { PanelId } from "./panelOrder";

/** What hovering the notch opens, in order: an agent that needs you, listed downloads (on top of the Shelf), the remembered panel, the hover panel, else status/music. */
export type HoverPick = "agent" | "downloads" | "remembered" | "hover-panel" | "default";

export function pickHoverPanel(o: {
  agentNeedsYou: boolean;
  /** Hovering opens a full panel: hover-open on, not Legacy Mode, no tour. */
  opensPanel: boolean;
  downloadsListed: boolean;
  available: readonly PanelId[];
  remembered: PanelId | null;
  hoverPanel: PanelId;
}): HoverPick {
  if (o.agentNeedsYou) return "agent";
  if (o.opensPanel && o.downloadsListed && o.available.includes("shelf")) return "downloads";
  if (o.remembered && o.available.includes(o.remembered)) return "remembered";
  if (o.opensPanel && o.available.includes(o.hoverPanel)) return "hover-panel";
  return "default";
}

/** Whether the panel the cursor left on is remembered for the next hover. */
export function shouldRemember(mode: PanelId | "agent", o: { overNotice: boolean; opensPanel: boolean; shelfByDownloads: boolean }): mode is PanelId {
  if (o.overNotice || mode === "agent") return false;
  if (mode === "shelf" && o.shelfByDownloads) return false;
  // Without hover-open the default is already status/music, so only the bigger panels are worth remembering.
  return o.opensPanel || mode === "command-center" || mode === "calendar";
}
