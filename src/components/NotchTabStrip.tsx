import { useEffect, useState } from "react";
import { motion } from "framer-motion";
import { Settings } from "lucide-react";
import { panelIcon } from "../panelIcons";
import { useT } from "../i18n";
import { EMPTY_PRIVACY, privacyChipWidth, type PrivacyItem, type PrivacyState } from "../privacyIndicator";
import { PrivacyChip, usePrivacyLines } from "./PrivacyIndicator";

/** Height of the strip in px; App adds it to the open notch height. */
export const TAB_STRIP_HEIGHT = 26;
const TAB_WIDTH = 24;

/** Narrowest notch width (px, including the notch's 12px side padding) that fits the strip. */
export function tabStripMinWidth(count: number) {
  return 24 + (count + 1) * TAB_WIDTH + 13;
}

interface NotchTabStripProps {
  panels: readonly string[];
  active: string;
  onSelect: (id: string) => void;
  onSettings: () => void;
  /** Mic/camera marks at the right edge (T-019); App widens the notch by tabStripReserve(items). */
  privacy?: { state: PrivacyState; items: PrivacyItem[]; onUnmute: () => void };
}

export function NotchTabStrip({ panels, active, onSelect, onSettings, privacy }: NotchTabStripProps) {
  const t = useT();
  const [hover, setHover] = useState(false);
  const [pinned, setPinned] = useState(false);
  const lines = usePrivacyLines(privacy?.state ?? EMPTY_PRIVACY, 3);
  const hasDots = !!privacy && privacy.items.some((i) => i !== "muted");
  useEffect(() => {
    if (hasDots) return;
    setHover(false);
    setPinned(false);
  }, [hasDots]);
  const detail = hasDots && (hover || pinned) ? lines.join(" · ") : "";
  return (
    <motion.div
      className={`notch-tabs${detail ? " showing-detail" : ""}`}
      role="tablist"
      onClick={(e) => e.stopPropagation()}
      initial={{ opacity: 0 }}
      animate={{ opacity: 1 }}
      exit={{ opacity: 0, transition: { duration: 0.08 } }}
      transition={{ duration: 0.15 }}
    >
      {panels.map((id) => {
        const Icon = panelIcon(id);
        const label = t(`panel.${id}`);
        const selected = id === active;
        return (
          <button
            key={id}
            type="button"
            role="tab"
            aria-selected={selected}
            aria-label={label}
            title={label}
            className={`notch-tab${selected ? " active" : ""}`}
            onClick={(e) => {
              e.stopPropagation();
              if (!selected) onSelect(id);
            }}
          >
            <Icon size={13} strokeWidth={2} />
          </button>
        );
      })}
      <span className="notch-tabs-sep" aria-hidden="true" />
      <button
        type="button"
        className="notch-tab"
        aria-label={t("cc.settings")}
        title={t("cc.settings")}
        onClick={(e) => {
          e.stopPropagation();
          onSettings();
        }}
      >
        <Settings size={13} strokeWidth={2} />
      </button>
      {detail && (
        <div className="notch-tabs-detail" role="status" style={{ right: privacyChipWidth(privacy!.items) + 6 }} title={detail}>
          {detail}
        </div>
      )}
      {privacy && privacy.items.length > 0 && (
        <PrivacyChip
          state={privacy.state}
          items={privacy.items}
          pinned={pinned}
          onHover={setHover}
          onTogglePin={() => setPinned((p) => !p)}
          onUnmute={privacy.onUnmute}
        />
      )}
    </motion.div>
  );
}
