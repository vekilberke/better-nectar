import { MicOff } from "lucide-react";
import { useT } from "../i18n";
import { privacyChipWidth, privacyLaneWidth, privacyLines, type PrivacyItem, type PrivacyState } from "../privacyIndicator";

export function usePrivacyLines(state: PrivacyState, max = 2): string[] {
  const t = useT();
  return privacyLines(state, { mic: t("privacy.mic"), camera: t("privacy.camera"), muted: t("privacy.muted"), partial: t("privacy.partial") }, max);
}

function MutedButton({ partial, onUnmute }: { partial: boolean; onUnmute: () => void }) {
  const t = useT();
  const label = partial ? `${t("privacy.partial")}. ${t("privacy.unmute")}` : t("privacy.unmute");
  return (
    <button
      type="button"
      className={partial ? "privacy-muted partial" : "privacy-muted"}
      aria-label={label}
      title={label}
      onPointerDown={(e) => e.stopPropagation()}
      onClick={(e) => {
        e.stopPropagation();
        onUnmute();
      }}
    >
      <MicOff size={13} strokeWidth={2.4} />
    </button>
  );
}

const Dot = ({ item }: { item: PrivacyItem }) => <span className={`privacy-dot ${item}`} aria-hidden="true" />;

/** Collapsed notch: dots (and the muted mic) in their own lane at the right edge of the main row. */
export function PrivacyLane({ state, items, onUnmute }: { state: PrivacyState; items: PrivacyItem[]; onUnmute: () => void }) {
  const lines = usePrivacyLines(state, 6);
  return (
    <div className="privacy-lane" style={{ width: privacyLaneWidth(items) }} title={lines.join("\n")} role="status" aria-label={lines.join(". ")}>
      {items.map((item) => (item === "muted" ? <MutedButton key={item} partial={state.partial} onUnmute={onUnmute} /> : <Dot key={item} item={item} />))}
    </div>
  );
}

interface PrivacyChipProps {
  state: PrivacyState;
  items: PrivacyItem[];
  pinned: boolean;
  onHover: (over: boolean) => void;
  onTogglePin: () => void;
  onUnmute: () => void;
}

/** Open notch: the same marks at the tab strip's right edge; hovering or clicking the dots names the apps. */
export function PrivacyChip({ state, items, pinned, onHover, onTogglePin, onUnmute }: PrivacyChipProps) {
  const t = useT();
  const lines = usePrivacyLines(state, 6);
  const dots = items.filter((i) => i !== "muted");
  return (
    <div
      className="privacy-chip"
      style={{ width: privacyChipWidth(items) }}
      onMouseEnter={() => onHover(true)}
      onMouseLeave={() => onHover(false)}
    >
      {state.muted && <MutedButton partial={state.partial} onUnmute={onUnmute} />}
      {dots.length > 0 && (
        <button
          type="button"
          className="privacy-dots"
          aria-label={lines.join(". ") || t("privacy.mic")}
          aria-pressed={pinned}
          onPointerDown={(e) => e.stopPropagation()}
          onClick={(e) => {
            e.stopPropagation();
            onTogglePin();
          }}
        >
          {dots.map((item) => (
            <Dot key={item} item={item} />
          ))}
        </button>
      )}
    </div>
  );
}
