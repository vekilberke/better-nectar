export type DockMode = "fixed" | "smart" | "peek" | "hybrid";

export const DOCK_MODES: DockMode[] = ["fixed", "smart", "peek", "hybrid"];

export interface DockVisibility {
  mode: string;
  fullscreen: boolean;
  shellSurface: boolean;
  overlapped: boolean;
  /** Hovered, a menu open, or still within the hide delay after one. */
  interacting: boolean;
  startupAnimating: boolean;
  /** Hybrid only: hidden with the AltGr tap. */
  hybridHidden: boolean;
}

/** Whether the dock slides away. Hybrid floats over windows and only the key (or fullscreen) hides it. */
export function isDockHidden(v: DockVisibility): boolean {
  if (v.mode !== "smart" && v.mode !== "peek" && v.mode !== "hybrid") return false;
  if (v.fullscreen || v.shellSurface) return true;
  if (v.mode === "hybrid") return v.hybridHidden;
  if (v.startupAnimating || v.interacting) return false;
  return v.mode === "peek" || (v.mode === "smart" && v.overlapped);
}

/** Next mode for the Control Center's dock tile. */
export function nextDockMode(mode: string): DockMode {
  const i = DOCK_MODES.indexOf(mode as DockMode);
  return DOCK_MODES[(i + 1) % DOCK_MODES.length];
}
