import { Activity, Bot, CalendarDays, Circle, ClipboardList, Inbox, Music, SlidersHorizontal, type LucideIcon } from "lucide-react";

/**
 * Icon for each notch panel, shared by the notch tab strip and Settings > Notch > Panel Order.
 * A new panel only needs an entry here (and a `panel.<id>` string in the locales);
 * panels without one still show, with a generic dot icon.
 */
export const PANEL_ICONS: Record<string, LucideIcon> = {
  "command-center": SlidersHorizontal,
  status: Activity,
  music: Music,
  calendar: CalendarDays,
  shelf: Inbox,
  clipboard: ClipboardList,
  agents: Bot,
};

export function panelIcon(id: string): LucideIcon {
  return PANEL_ICONS[id] ?? Circle;
}
