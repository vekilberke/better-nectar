import { describe, expect, test } from "bun:test";
import { createNoticeQueue, bodyLines, canShow, pickNotice, stackShownId, showDuration, MAX_QUEUE, MAX_WAIT_MS, SHOW_MS, SHOW_MS_QUEUED, type NotchNotice } from "./notchNotifications";

const notice = (id: number, over: Partial<NotchNotice> = {}): NotchNotice => ({ id, app: "Outlook", aumid: "outlook", title: `t${id}`, body: "", time: id, ...over });

describe("notch notice queue", () => {
  test("shows in arrival order and counts the waiting ones", () => {
    const q = createNoticeQueue();
    q.push(notice(1));
    q.push(notice(2));
    q.push(notice(3));
    expect(q.current()?.id).toBe(1);
    expect(q.waiting()).toBe(2);
    expect(q.advance()?.id).toBe(2);
    expect(q.waiting()).toBe(1);
    q.advance();
    expect(q.advance()).toBeNull();
    expect(q.waiting()).toBe(0);
  });

  test("the same id refreshes the notice instead of queueing it twice", () => {
    const q = createNoticeQueue();
    expect(q.push(notice(1))).toBe(true);
    expect(q.push(notice(1, { title: "updated" }))).toBe(false);
    expect(q.size()).toBe(1);
    expect(q.current()?.title).toBe("updated");
  });

  test("removing the notice on screen moves on to the next", () => {
    const q = createNoticeQueue();
    q.push(notice(1));
    q.push(notice(2));
    expect(q.remove(1)).toBe(true);
    expect(q.current()?.id).toBe(2);
    expect(q.remove(99)).toBe(false);
  });

  test("a flood keeps the card on screen and drops the oldest waiting ones", () => {
    const q = createNoticeQueue();
    for (let i = 1; i <= MAX_QUEUE + 5; i++) q.push(notice(i));
    expect(q.size()).toBe(MAX_QUEUE);
    expect(q.current()?.id).toBe(1);
    q.advance();
    expect(q.current()?.id).toBe(7);
  });

  test("clear empties everything", () => {
    const q = createNoticeQueue();
    q.push(notice(1));
    q.clear();
    expect(q.current()).toBeNull();
  });
});

describe("notch notice display", () => {
  test("cards get less time when others wait", () => {
    expect(showDuration(0)).toBe(SHOW_MS);
    expect(showDuration(3)).toBe(SHOW_MS_QUEUED);
  });

  test("body lines follow length and line breaks, up to three", () => {
    expect(bodyLines("")).toBe(0);
    expect(bodyLines("  ")).toBe(0);
    expect(bodyLines("Lunch?")).toBe(1);
    expect(bodyLines("a\nb")).toBe(2);
    expect(bodyLines("x".repeat(400))).toBe(3);
  });

  test("a card never pops over a notch the user already opened, but hovering keeps one open", () => {
    expect(canShow({ blocked: false, open: false, alreadyShowing: false })).toBe(true);
    expect(canShow({ blocked: false, open: true, alreadyShowing: false })).toBe(false);
    expect(canShow({ blocked: false, open: true, alreadyShowing: true })).toBe(true);
    expect(canShow({ blocked: true, open: false, alreadyShowing: true })).toBe(false);
  });
});

describe("notch notice queue edge cases", () => {
  test("advancing or removing on an empty queue is harmless", () => {
    const q = createNoticeQueue();
    expect(q.advance()).toBeNull();
    expect(q.remove(1)).toBe(false);
    expect(q.waiting()).toBe(0);
    expect(q.current()).toBeNull();
  });

  test("a refresh keeps the arrival position and never drops anything when the queue is full", () => {
    const q = createNoticeQueue();
    for (let i = 1; i <= MAX_QUEUE; i++) q.push(notice(i));
    expect(q.push(notice(5, { title: "again" }))).toBe(false);
    expect(q.size()).toBe(MAX_QUEUE);
    for (let i = 1; i < 5; i++) q.advance();
    expect(q.current()?.id).toBe(5);
    expect(q.current()?.title).toBe("again");
  });

  test("removing a waiting notice keeps the order of the rest", () => {
    const q = createNoticeQueue();
    [1, 2, 3, 4].forEach((i) => q.push(notice(i)));
    q.remove(3);
    expect(q.waiting()).toBe(2);
    expect(q.advance()?.id).toBe(2);
    expect(q.advance()?.id).toBe(4);
  });

  test("a notice can be pushed again after it was dismissed", () => {
    const q = createNoticeQueue();
    q.push(notice(1));
    q.remove(1);
    expect(q.push(notice(1))).toBe(true);
    expect(q.current()?.id).toBe(1);
  });

  test("the flood limit holds when the on-screen card is the only one that survives repeatedly", () => {
    const q = createNoticeQueue();
    for (let i = 1; i <= MAX_QUEUE * 3; i++) q.push(notice(i));
    expect(q.size()).toBe(MAX_QUEUE);
    expect(q.current()?.id).toBe(1);
    // The newest one is always kept.
    let last = q.current();
    for (let n = q.size(); n > 0; n--) last = q.current() ?? last, q.advance();
    expect(last?.id).toBe(MAX_QUEUE * 3);
  });
});

describe("notch notice display edge cases", () => {
  test("body line estimate at the wrapping boundary", () => {
    expect(bodyLines("x".repeat(50))).toBe(1);
    expect(bodyLines("x".repeat(51))).toBe(2);
    expect(bodyLines("a\n\nb")).toBe(3);
    expect(bodyLines("a\nb\nc\nd\ne")).toBe(3);
    expect(bodyLines("\n")).toBe(0);
  });

  test("a blocked notch never shows a card, whatever else is true", () => {
    for (const open of [true, false]) for (const alreadyShowing of [true, false]) expect(canShow({ blocked: true, open, alreadyShowing })).toBe(false);
  });

  test("durations", () => {
    expect(showDuration(1)).toBe(SHOW_MS_QUEUED);
    expect(SHOW_MS_QUEUED).toBeLessThan(SHOW_MS);
  });
});

describe("notch notice backlog", () => {
  test("stale waiting notices are dropped, the one on screen stays", () => {
    const q = createNoticeQueue();
    const now = 10 * MAX_WAIT_MS;
    q.push(notice(1, { time: now - MAX_WAIT_MS - 5000 }));
    q.push(notice(2, { time: now - MAX_WAIT_MS - 1 }));
    q.push(notice(3, { time: now - MAX_WAIT_MS }));
    q.push(notice(4, { time: now }));
    expect(q.dropStale(now, 1)).toBe(true);
    expect(q.current()?.id).toBe(1);
    expect(q.waiting()).toBe(2);
    q.advance();
    expect(q.current()?.id).toBe(3);
    expect(q.dropStale(now, null)).toBe(false);
  });

  test("a long block leaves only fresh notices", () => {
    const q = createNoticeQueue();
    for (let i = 1; i <= MAX_QUEUE; i++) q.push(notice(i, { time: i * 1000 }));
    q.dropStale(MAX_QUEUE * 1000 + MAX_WAIT_MS - 1500, null);
    expect(q.size()).toBe(2);
    expect(q.current()?.id).toBe(MAX_QUEUE - 1);
  });
});

describe("notch notice queue round 2", () => {
  test("pushing into a full queue still reports a new notice and evicts the oldest waiting one", () => {
    const q = createNoticeQueue();
    for (let i = 1; i <= MAX_QUEUE; i++) q.push(notice(i));
    expect(q.push(notice(100))).toBe(true);
    expect(q.size()).toBe(MAX_QUEUE);
    expect(q.current()?.id).toBe(1);
    q.advance();
    expect(q.current()?.id).toBe(3);
  });

  test("advancing the last notice empties the queue", () => {
    const q = createNoticeQueue();
    q.push(notice(1));
    expect(q.advance()).toBeNull();
    expect(q.current()).toBeNull();
    expect(q.waiting()).toBe(0);
    expect(q.size()).toBe(0);
  });

  test("notices stamped in the future are never stale", () => {
    const q = createNoticeQueue();
    q.push(notice(1, { time: 0 }));
    q.push(notice(2, { time: 10 * MAX_WAIT_MS }));
    expect(q.dropStale(5000, null)).toBe(false);
    expect(q.size()).toBe(2);
  });

  test("a keep id that is not queued protects nothing", () => {
    const q = createNoticeQueue();
    q.push(notice(1, { time: 0 }));
    expect(q.dropStale(MAX_WAIT_MS + 1, 99)).toBe(true);
    expect(q.size()).toBe(0);
  });

  test("removing an unknown id reports false and changes nothing", () => {
    const q = createNoticeQueue();
    q.push(notice(1));
    expect(q.remove(42)).toBe(false);
    expect(q.size()).toBe(1);
  });
});

describe("notch notice display round 2", () => {
  test("body lines for blank, whitespace-only and Windows line endings", () => {
    expect(bodyLines("")).toBe(0);
    expect(bodyLines("   \n  ")).toBe(0);
    expect(bodyLines("a\r\nb")).toBe(2);
    expect(bodyLines("a\n\nb")).toBe(3);
    expect(bodyLines("x".repeat(5000))).toBe(3);
  });

  test("canShow covers every combination", () => {
    for (const blocked of [true, false]) {
      for (const open of [true, false]) {
        for (const alreadyShowing of [true, false]) {
          const want = !blocked && (alreadyShowing || !open);
          expect(canShow({ blocked, open, alreadyShowing })).toBe(want);
        }
      }
    }
  });
});

describe("moving through the stack on an open notch", () => {
  const open = { blocked: false, open: true, now: 10 };

  test("+N while the cursor is on the card shows the next card", () => {
    const q = createNoticeQueue();
    q.push(notice(1));
    q.push(notice(2));
    let shown = pickNotice(q, null, { ...open, open: false })?.id ?? null;
    expect(shown).toBe(1);
    expect(pickNotice(q, shown, open)?.id).toBe(1);
    q.advance();
    shown = stackShownId(q, shown, true);
    expect(pickNotice(q, shown, open)?.id).toBe(2);
  });

  test("dismissing the last card ends the stack", () => {
    const q = createNoticeQueue();
    q.push(notice(1));
    q.advance();
    const shown = stackShownId(q, 1, true);
    expect(shown).toBeNull();
    q.push(notice(3));
    expect(pickNotice(q, shown, open)).toBeNull();
  });

  test("a timeout does not start the next card over an open notch", () => {
    const q = createNoticeQueue();
    q.push(notice(1));
    q.push(notice(2));
    q.advance();
    const shown = stackShownId(q, 1, false);
    expect(pickNotice(q, shown, open)).toBeNull();
    expect(pickNotice(q, shown, { ...open, open: false })?.id).toBe(2);
  });

  test("blocked still wins over a card handed on by the user", () => {
    const q = createNoticeQueue();
    q.push(notice(1));
    q.push(notice(2));
    q.advance();
    expect(pickNotice(q, stackShownId(q, 1, true), { ...open, blocked: true })).toBeNull();
  });

  test("a stale card is skipped when it would start fresh, not when the user steps to it", () => {
    const q = createNoticeQueue();
    q.push(notice(1, { time: 0 }));
    q.push(notice(2, { time: 0 }));
    q.push(notice(3, { time: MAX_WAIT_MS + 5 }));
    const now = MAX_WAIT_MS + 10;
    expect(pickNotice(q, null, { blocked: false, open: false, now })?.id).toBe(3);
    const q2 = createNoticeQueue();
    q2.push(notice(1, { time: now }));
    q2.push(notice(2, { time: 0 }));
    q2.advance();
    expect(pickNotice(q2, stackShownId(q2, 1, true), { blocked: false, open: false, now })?.id).toBe(2);
  });
});

describe("notch notice queue, round 3", () => {
  test("a notice exactly MAX_WAIT_MS old is still fresh, one millisecond more is stale", () => {
    const q = createNoticeQueue();
    q.push(notice(1, { time: 1000 }));
    q.push(notice(2, { time: 1000 }));
    q.push(notice(3, { time: 999 }));
    const now = 1000 + MAX_WAIT_MS;
    expect(q.dropStale(now, 1)).toBe(true);
    expect([q.current()?.id, q.size()]).toEqual([1, 2]);
    expect(q.dropStale(now, 1)).toBe(false);
    expect(q.dropStale(now + 1, 1)).toBe(true);
    expect(q.size()).toBe(1);
  });

  test("an open notch never drops stale notices (they are judged when the card would start)", () => {
    const q = createNoticeQueue();
    q.push(notice(1, { time: 0 }));
    expect(pickNotice(q, null, { blocked: false, open: true, now: MAX_WAIT_MS * 10 })).toBeNull();
    expect(q.size()).toBe(1);
    expect(pickNotice(q, null, { blocked: true, open: false, now: MAX_WAIT_MS * 10 })).toBeNull();
    expect(q.size()).toBe(1);
  });

  test("an empty queue never picks anything and stays harmless", () => {
    const q = createNoticeQueue();
    expect(pickNotice(q, null, { blocked: false, open: false, now: 0 })).toBeNull();
    expect(pickNotice(q, 5, { blocked: false, open: true, now: 0 })).toBeNull();
    expect(stackShownId(q, 5, true)).toBeNull();
    expect(stackShownId(q, 5, false)).toBe(5);
    expect(q.dropStale(0, null)).toBe(false);
  });

  test("a full stack walked through with +N shows every card once, in order", () => {
    const q = createNoticeQueue();
    for (let i = 1; i <= 5; i++) q.push(notice(i, { time: 100 }));
    const seen: number[] = [];
    let shown: number | null = null;
    const opts = { blocked: false, open: false, now: 100 };
    for (let guard = 0; guard < 10; guard++) {
      const card = pickNotice(q, shown, opts);
      if (!card) break;
      shown = card.id;
      seen.push(card.id);
      q.advance();
      shown = stackShownId(q, shown, true);
    }
    expect(seen).toEqual([1, 2, 3, 4, 5]);
    expect(q.size()).toBe(0);
  });

  test("Windows removing a waiting notice while another card shows leaves the card on screen alone", () => {
    const q = createNoticeQueue();
    q.push(notice(1));
    q.push(notice(2));
    q.push(notice(3));
    expect(q.remove(2)).toBe(true);
    expect(q.current()?.id).toBe(1);
    expect(q.waiting()).toBe(1);
    expect(pickNotice(q, 1, { blocked: false, open: true, now: 3 })?.id).toBe(1);
  });

  test("a card the user is looking at survives a flood and the refreshed copy keeps the slot", () => {
    const q = createNoticeQueue();
    q.push(notice(1));
    for (let i = 2; i <= MAX_QUEUE + 10; i++) q.push(notice(i));
    expect(q.size()).toBe(MAX_QUEUE);
    expect(q.current()?.id).toBe(1);
    expect(q.push(notice(1, { title: "newer text" }))).toBe(false);
    expect(q.current()?.title).toBe("newer text");
    expect(q.size()).toBe(MAX_QUEUE);
  });

  test("durations never drop below the queued length and never go negative for odd counts", () => {
    expect(showDuration(-1)).toBe(SHOW_MS);
    expect(showDuration(Number.MAX_SAFE_INTEGER)).toBe(SHOW_MS_QUEUED);
    expect(SHOW_MS_QUEUED).toBeLessThan(SHOW_MS);
  });

  test("body lines for unicode, tabs and a blank-only body", () => {
    expect(bodyLines("\n")).toBe(0);
    expect(bodyLines("\ta")).toBe(1);
    expect(bodyLines("é".repeat(50))).toBe(1);
    expect(bodyLines("é".repeat(51))).toBe(2);
    expect(bodyLines("x".repeat(100000))).toBe(3);
  });
});
