import { CAT_PALETTE_NAMES } from "./components/sonderCatData";

export const DEFAULT_CAT_PALETTE = "orange tabby";

export function parseCatPalette(val: string | null | undefined): string {
  return val && CAT_PALETTE_NAMES.includes(val) ? val : DEFAULT_CAT_PALETTE;
}

export type NotchIdle = "clock" | "cat" | "hybrid";

/** nectar-notch-idle; unset (new installs) means the cat beside a 24-hour clock. */
export function parseNotchIdle(val: unknown): NotchIdle {
  return val === "clock" || val === "cat" ? val : "hybrid";
}
