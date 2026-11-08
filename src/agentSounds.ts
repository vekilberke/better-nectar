import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export type SoundState = "needs_input" | "done" | "error";

const cache = new Map<SoundState, string>();

const STATES: SoundState[] = ["needs_input", "done", "error"];
const warm = () => STATES.forEach((state) => { sourceFor(state); });

listen("agent-sound-changed", () => { cache.clear(); warm(); }).catch(() => { });
setTimeout(warm, 0);

const defaultSource = (state: SoundState) => `/sounds/agent-${state}.wav`;

/** The cat's meow (T-022): replaces the built-in needs-input sound while the cat is on the notch. */
export const MEOW_SOUND = "/sounds/cat-meow.wav";

async function sourceFor(state: SoundState) {
  const cached = cache.get(state);
  if (cached) return cached;
  const custom = await invoke<string | null>("agent_sound_data", { state }).catch(() => null);
  const source = custom ?? defaultSource(state);
  cache.set(state, source);
  return source;
}

/** fallback replaces the built-in sound only; a sound the user picked for this state still wins. */
export async function playAgentSound(state: SoundState, force = false, fallback?: string) {
  try {
    if (!force && localStorage.getItem("nectar-agent-sounds") === "false") return;
    const source = await sourceFor(state);
    const audio = new Audio(fallback && source === defaultSource(state) ? fallback : source);
    audio.volume = 0.6;
    await audio.play();
  } catch {
  }
}
