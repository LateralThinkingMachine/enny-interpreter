import {usd} from "./cost.js";
import type {CostNotificationPort, CostNotificationState} from "./ports.js";

export type CostNotice = Readonly<{milestone_microusd: number; crossed_intervals: number}>;

/** A page starts at the server's current counter; old milestones never replay. */
export class CostMilestones {
  private interval: number | null = null;
  private highest = 0;
  observe(interval: number, allocated: number): CostNotice | null {
    if (!Number.isSafeInteger(interval) || interval <= 0 || !Number.isSafeInteger(allocated) || allocated < 0) throw new Error("Invalid cost counter");
    if (this.interval !== null && this.interval !== interval) throw new Error("Changed cost interval");
    const reached = Math.floor(allocated / interval);
    if (this.interval === null) {this.interval = interval; this.highest = reached; return null;}
    if (reached <= this.highest) return null;
    const crossed_intervals = reached - this.highest;
    this.highest = reached;
    return {milestone_microusd: reached * interval, crossed_intervals};
  }
}
export function costNoticeText(notice: CostNotice): string {
  return `Estimated speech allocations since this local service started reached US${usd(notice.milestone_microusd)}.${notice.crossed_intervals > 1 ? ` ${notice.crossed_intervals} warning intervals crossed since the last notice.` : ""} Not measured account spend. Warnings do not stop AI or cap spending.`;
}
export function nextCostMilestone(interval: number, allocated: number): number | null {
  const next = (Math.floor(allocated / interval) + 1) * interval;
  return Number.isSafeInteger(next) ? next : null;
}

type BrowserNotice = {close(): void};
type NotificationApi = {
  new(title: string, options: NotificationOptions): BrowserNotice;
  readonly permission: NotificationPermission;
  requestPermission(): Promise<NotificationPermission>;
};
/** One owned, silent, amount-only browser notice. No click actions or content. */
export function browserCostNotifications(api: NotificationApi | undefined): CostNotificationPort {
  let current: BrowserNotice | null = null;
  const close = () => {const notice = current; current = null; try {notice?.close();} catch { /* Notification cleanup cannot affect media cleanup. */ }};
  const status = (): CostNotificationState => {
    try {return api && ["default", "granted", "denied"].includes(api.permission) ? api.permission : "unavailable";} catch {return "unavailable";}
  };
  return {
    status,
    async requestPermission() {
      if (status() !== "default" || !api) return status();
      try {await api.requestPermission(); return status();} catch {return "unavailable";}
    },
    notify(message) {
      if (status() !== "granted" || !api) return;
      close();
      current = new api("Enny · Cost notice", {body: message, tag: "meeting-companion-session-cost", silent: true});
    },
    close,
  };
}
