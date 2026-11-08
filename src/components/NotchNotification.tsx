import { AnimatePresence, motion } from "framer-motion";
import { Bell, X } from "lucide-react";
import { timeAgo, useT } from "../i18n";
import { bodyLines, type NotchNotice } from "../notchNotifications";

export const NOTICE_WIDTH = 380;

/** Height of the open notch for a notice; the body gets 0 to 3 lines. */
export function noticeHeight(notice: NotchNotice): number {
  return 70 + bodyLines(notice.body) * 16;
}

interface Props {
  notice: NotchNotice;
  waiting: number;
  onOpen: () => void;
  onDismiss: () => void;
  onNext: () => void;
}

export function NotchNotificationCard({ notice, waiting, onOpen, onDismiss, onNext }: Props) {
  const t = useT();
  const lines = bodyLines(notice.body);
  const stop = (fn: () => void) => (e: React.MouseEvent) => {
    e.stopPropagation();
    fn();
  };

  return (
    <motion.div
      className="notch-notice"
      initial={{ opacity: 0, y: -8, filter: "blur(6px)" }}
      animate={{ opacity: 1, y: 0, filter: "blur(0px)" }}
      exit={{ opacity: 0, y: -6, filter: "blur(6px)", transition: { duration: 0.12 } }}
      transition={{ type: "spring", stiffness: 500, damping: 34 }}
    >
      <AnimatePresence mode="popLayout" initial={false}>
        <motion.div
          key={notice.id}
          className="notch-notice-inner"
          role="button"
          tabIndex={0}
          title={t("notif.open")}
          onClick={stop(onOpen)}
          onKeyDown={(e) => { if (e.key === "Enter") onOpen(); }}
          initial={{ opacity: 0, x: 28 }}
          animate={{ opacity: 1, x: 0 }}
          exit={{ opacity: 0, x: -28, transition: { duration: 0.12 } }}
          transition={{ type: "spring", stiffness: 520, damping: 36 }}
        >
          <span className="notch-notice-icon">
            {notice.icon ? <img src={notice.icon} alt="" draggable={false} /> : <Bell size={16} strokeWidth={1.75} />}
          </span>
          <div className="notch-notice-text">
            <div className="notch-notice-head">
              <span className="notch-notice-app">{notice.app}</span>
              <span className="notch-notice-time">{timeAgo(notice.time, t)}</span>
              {waiting > 0 && (
                <button className="notch-notice-more" onClick={stop(onNext)} title={t("notif.next")}>
                  +{waiting}
                </button>
              )}
              <button className="notch-notice-close" onClick={stop(onDismiss)} title={t("notif.dismiss")} aria-label={t("notif.dismiss")}>
                <X size={12} strokeWidth={2.25} />
              </button>
            </div>
            <b className="notch-notice-title">{notice.title || notice.app}</b>
            {lines > 0 && (
              <span className="notch-notice-body" style={{ WebkitLineClamp: lines }}>
                {notice.body}
              </span>
            )}
          </div>
        </motion.div>
      </AnimatePresence>
    </motion.div>
  );
}
