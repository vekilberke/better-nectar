/** `nectar-welcome-done`: unset on a fresh profile, "pending" while the welcome screen waits, "true" after. */
export const WELCOME_KEY = "nectar-welcome-done";

export type WelcomeInput = {
  /** The saved `nectar-welcome-done` value. */
  setting: unknown;
  /** localStorage `nectar-first-run` was set by an earlier start. */
  firstRunDone: boolean;
  /** `nectar-tour-seen` is "1" in localStorage or settings.json. */
  tourSeen: boolean;
};

/**
 * "done": nothing to do. "adopt": an install from before the welcome screen; mark it done silently
 * so the owner never sees it. "show": hold the dock and autostart until the user chooses.
 */
export function welcomeState({ setting, firstRunDone, tourSeen }: WelcomeInput): "done" | "adopt" | "show" {
  if (setting === "true" || setting === true) return "done";
  if (setting === "pending") return "show";
  return firstRunDone || tourSeen ? "adopt" : "show";
}

/** 24-hour clock for locales that use it (follows the locale, Q16). */
export function localeUses24h(locale: string): boolean {
  try {
    return new Intl.DateTimeFormat(locale, { hour: "numeric" }).resolvedOptions().hour12 === false;
  } catch {
    return false;
  }
}
