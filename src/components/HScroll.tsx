import { useCallback, useEffect, useState, type RefObject } from "react";
import { ChevronLeft, ChevronRight } from "lucide-react";

/** Room left for the neighbouring item under an arrow after paging (arrow 30px at 4px from the edge, plus 4px). */
const PEEK = 38;

/** Tracks whether a horizontal row can scroll either way; a vertical wheel scrolls it sideways. */
export function useHScroll(ref: RefObject<HTMLElement | null>, deps: unknown[]) {
  const [edges, setEdges] = useState({ prev: false, next: false });

  const measure = useCallback(() => {
    const el = ref.current;
    if (!el) return;
    const prev = el.scrollLeft > 2;
    const next = el.scrollLeft + el.clientWidth < el.scrollWidth - 2;
    setEdges((cur) => (cur.prev === prev && cur.next === next ? cur : { prev, next }));
  }, [ref]);

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    measure();
    const onWheel = (e: WheelEvent) => {
      if (el.scrollWidth <= el.clientWidth + 1 || Math.abs(e.deltaX) > Math.abs(e.deltaY)) return;
      el.scrollLeft += e.deltaY;
    };
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    // An exiting item leaves the DOM after its exit animation, with no resize or dependency change to measure on.
    const mo = new MutationObserver(measure);
    mo.observe(el, { childList: true });
    el.addEventListener("scroll", measure, { passive: true });
    el.addEventListener("wheel", onWheel, { passive: true });
    return () => {
      ro.disconnect();
      mo.disconnect();
      el.removeEventListener("scroll", measure);
      el.removeEventListener("wheel", onWheel);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [measure, ...deps]);

  // Pages to item edges, so an item always starts just right of the back arrow with the one before it peeking
  // underneath; content is left-aligned, so the arrows cover the right side of items, not their start.
  const page = useCallback((dir: 1 | -1) => {
    const el = ref.current;
    if (!el) return;
    const box = el.getBoundingClientRect();
    const kids = Array.from(el.children).map((c) => c.getBoundingClientRect()).filter((r) => r.width > 0);
    const starts = kids.map((r) => el.scrollLeft + r.left - box.left - PEEK);
    let left: number;
    if (dir === 1) {
      const cut = kids.findIndex((r) => r.right > box.right - 1);
      left = cut > 0 && starts[cut] > el.scrollLeft ? starts[cut] : el.scrollLeft + el.clientWidth * 0.75;
    } else {
      const back = el.scrollLeft - (el.clientWidth - 2 * PEEK);
      left = back <= 0 ? 0 : (starts.find((s) => s >= back && s < el.scrollLeft - 1) ?? back);
    }
    const reduce = window.matchMedia?.("(prefers-reduced-motion: reduce)").matches;
    el.scrollTo({ left: Math.max(0, left), behavior: reduce ? "auto" : "smooth" });
  }, [ref]);

  return { ...edges, page };
}

type ArrowsProps = { prev: boolean; next: boolean; page: (dir: 1 | -1) => void; labels: { prev: string; next: string } };

export function HScrollArrows({ prev, next, page, labels }: ArrowsProps) {
  return (
    <>
      {prev && (
        <button type="button" className="hs-arrow prev" aria-label={labels.prev} title={labels.prev} onPointerDown={(e) => e.stopPropagation()} onClick={(e) => { e.stopPropagation(); page(-1); }}>
          <ChevronLeft size={16} strokeWidth={2.4} aria-hidden="true" />
        </button>
      )}
      {next && (
        <button type="button" className="hs-arrow next" aria-label={labels.next} title={labels.next} onPointerDown={(e) => e.stopPropagation()} onClick={(e) => { e.stopPropagation(); page(1); }}>
          <ChevronRight size={16} strokeWidth={2.4} aria-hidden="true" />
        </button>
      )}
    </>
  );
}
