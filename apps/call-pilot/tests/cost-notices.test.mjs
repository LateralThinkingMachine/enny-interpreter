import assert from "node:assert/strict";
import test from "node:test";
import {CostMilestones, browserCostNotifications, costNoticeText, nextCostMilestone} from "../.test-build/cost-notices.js";

test("exact recurring milestones are monotonic, coalesced and bounded at safe integer capacity", () => {
  const milestones = new CostMilestones();
  assert.equal(milestones.observe(5_000_000, 0), null);
  assert.equal(milestones.observe(5_000_000, 4_999_999), null);
  assert.deepEqual(milestones.observe(5_000_000, 5_000_000), {milestone_microusd: 5_000_000, crossed_intervals: 1});
  assert.equal(milestones.observe(5_000_000, 5_000_000), null);
  assert.deepEqual(milestones.observe(5_000_000, 27_000_000), {milestone_microusd: 25_000_000, crossed_intervals: 4});
  assert.equal(milestones.observe(5_000_000, 10_000_000), null);
  assert.equal(milestones.observe(5_000_000, 27_000_000), null);
  assert.throws(() => milestones.observe(1_000_000, 30_000_000));
  for (const [interval, amount] of [[0, 0], [1, -1], [NaN, 0], [1, Number.MAX_SAFE_INTEGER + 1]]) assert.throws(() => new CostMilestones().observe(interval, amount));
  const maximum = new CostMilestones(); maximum.observe(1, 0);
  const notice = maximum.observe(1, Number.MAX_SAFE_INTEGER);
  assert.equal(notice.crossed_intervals, Number.MAX_SAFE_INTEGER); assert.ok(costNoticeText(notice).length < 300);
  assert.equal(nextCostMilestone(1, Number.MAX_SAFE_INTEGER), null);
  assert.equal(nextCostMilestone(5_000_000, 5_000_000), 10_000_000);
});

test("initial baseline never replays an old process milestone", () => {
  const milestones = new CostMilestones(); assert.equal(milestones.observe(5_000_000, 16_000_000), null);
  assert.deepEqual(milestones.observe(5_000_000, 20_000_000), {milestone_microusd: 20_000_000, crossed_intervals: 1});
});

test("notice wording identifies the accumulated service lifetime without claiming billed spend", () => {
  const message = costNoticeText({milestone_microusd: 10_000_000, crossed_intervals: 2});
  assert.match(message, /^Estimated speech allocations since this local service started reached US\$10\.00\./);
  assert.match(message, /2 warning intervals crossed since the last notice/);
  assert.match(message, /Not measured account spend/);
  assert.match(message, /Not measured account spend/);
  assert.match(message, /Warnings do not stop AI or cap spending/);
  assert.doesNotMatch(message, /session cost|session allocation/);
});

test("browser adapter owns at most one silent cost notice with no actions, remote resources or speech", async () => {
  const seen = []; let closed = 0, permissionRequests = 0;
  class FakeNotification {
    static permission = "default";
    static async requestPermission() {permissionRequests++; this.permission = "granted"; return this.permission;}
    constructor(title, options) {seen.push({title, options});}
    close() {closed++;}
  }
  const port = browserCostNotifications(FakeNotification);
  assert.equal(port.status(), "default"); port.notify("hidden"); assert.equal(seen.length, 0); assert.equal(permissionRequests, 0);
  assert.equal(await port.requestPermission(), "granted"); assert.equal(permissionRequests, 1);
  const first = costNoticeText({milestone_microusd: 5_000_000, crossed_intervals: 1});
  port.notify(first); port.notify(costNoticeText({milestone_microusd: 10_000_000, crossed_intervals: 1}));
  assert.equal(seen.length, 2); assert.equal(closed, 1);
  assert.deepEqual(seen[0], {title: "Enny · Cost notice", options: {body: first, tag: "meeting-companion-session-cost", silent: true}});
  port.close(); port.close(); assert.equal(closed, 2);
  FakeNotification.permission = "denied"; assert.equal(await port.requestPermission(), "denied"); port.notify("hidden"); assert.equal(seen.length, 2); assert.equal(permissionRequests, 1);
});

test("absent or failing Notification APIs remain optional", async () => {
  const absent = browserCostNotifications(undefined); assert.equal(absent.status(), "unavailable"); assert.equal(await absent.requestPermission(), "unavailable"); absent.notify("hidden"); absent.close();
  const failing = browserCostNotifications({permission: "default", requestPermission: async () => {throw new Error("Synthetic denial");}});
  assert.equal(await failing.requestPermission(), "unavailable"); failing.notify("hidden"); failing.close();
});
