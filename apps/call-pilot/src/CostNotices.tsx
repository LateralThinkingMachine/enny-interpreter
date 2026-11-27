import type {View} from "./client.js";
import {usd} from "./cost.js";
import {costNoticeText, nextCostMilestone} from "./cost-notices.js";

const DELIVERY: Record<View["costNotificationState"], string> = {
  default: "Your browser may ask to allow cost notifications when you press Start. Speech starts independently of this choice.",
  requesting: "Your browser’s notification choice is pending. Cost notices remain visible here and speech can start.",
  granted: "Browser cost notifications are allowed. Delivery during your call still depends on your browser and your system notification settings.",
  denied: "Browser notifications are blocked for this site. Cost notices remain here; allow notifications in your browser’s site settings for notices during your call.",
  unavailable: "Browser notifications are unavailable. Cost notices remain visible here.",
};

export function CostNotices({view}: {view: View}) {
  const cost = view.budget?.continuous;
  if (view.role !== "operator" || !view.session?.continuous || !view.session.providers_enabled || !cost || view.browserListening) return null;
  const inactive = view.aiPaused || !!view.manualPause || view.finishing;
  const next = nextCostMilestone(cost.warning_threshold_microusd, cost.allocated_microusd);
  return <div className="meet-cost-notices" aria-label="Recurring cost notices">
    <div className="meet-cost-summary"><strong>Cost notices every US{usd(cost.warning_threshold_microusd)}</strong><span>{view.budgetFresh ? "Estimated speech allocations since this local service started" : "Last observed speech allocations · current total unverified"}: US{usd(cost.allocated_microusd)}.</span></div>
    <p className="meet-cost-next">{!view.budgetFresh ? "The current estimate is unverified; this retained amount does not show current processing or cost closure." : inactive ? "This estimate is retained for review." : next === null ? "The next notice amount cannot be represented." : `Next notice at US${usd(next)}.`} No total spending cap.</p>
    <p id="cost-milestone-notice" role="status" aria-live="polite" aria-atomic="true">{view.costNotice ? costNoticeText(view.costNotice) : ""}</p>
    <details className="meet-details cost-notice-details"><summary>About costs &amp; notifications</summary>
      <p>These speech estimates include opened windows and failed attempts; they are not measured account charges.</p>
      <p>{inactive ? "Browser cost notices are closed while AI is paused or finishing. Any retained milestone describes an earlier estimate." : DELIVERY[view.costNotificationState]}</p>
      <p>Starting a new session keeps these allocations. Estimates and notice milestones reset when the local service restarts. Notices are amount-only and silent. A new session or page reload starts from the current estimate without replaying old notices. Browser notices stop when you end this session.</p>
    </details>
  </div>;
}
