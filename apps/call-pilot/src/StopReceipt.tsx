import type {PilotClient, View} from "./client.js";
import {usd} from "./cost.js";
import {RustSurface} from "./rust-surface.js";

export function StopReceiptPanel({view, client}: {view: View; client: PilotClient}) {
  if (!view.stopped || !view.session || view.role !== "operator") return null;
  const receipt = view.stopReceipt;
  const local = {checking: "Checking this page’s guided audio handles…", confirmed: "Guided audio handles retired on this page.", unconfirmed: "Guided audio retirement unconfirmed."};
  const rtc = {unused: "This page opened no private WebRTC audio.", confirmed: "Private WebRTC audio had already retired.", unconfirmed: "Private WebRTC audio retirement unconfirmed."};
  const activity = {none_admitted: "No provider activity admitted.", close_observed: "Closure observed by the service.", unconfirmed: "Provider closure unconfirmed. Processing may still incur charges."};
  const data = {
    audio: local[view.localAudioRetirement], rtc: rtc[view.rtcAudioRetirement],
    ack: view.stopAcknowledged ? "Stop acknowledged by the server." : "No verified Stop acknowledgment.",
    transport: receipt ? activity[receipt.provider_activity] : view.stopReceiptState === "checking" ? "Checking the service…" : "Provider activity unavailable.",
    reserved: receipt?.reserved_microusd != null ? usd(receipt.reserved_microusd) + " reserved locally." : "Reservation unavailable.",
    invoice: "Actual charges unavailable. Check your provider account.",
    checking: view.stopReceiptState === "checking" || view.stopReceiptState === "unauthorized",
  };
  return <RustSurface kind="stop-receipt" state={data} dispatch={action => {if (action === "check") void client.recheckStopEvidence();}} fallback={
    <section className="stop-receipt" aria-labelledby="stop-receipt-heading">
      <h2 id="stop-receipt-heading">Stop receipt</h2>
      <dl><dt>Browser audio</dt><dd>{data.audio}</dd><dt>Private WebRTC audio</dt><dd>{data.rtc}</dd>
        <dt>Server Stop</dt><dd>{data.ack}</dd><dt>Provider transport</dt><dd>{data.transport}</dd>
        <dt>Local reservation</dt><dd>{data.reserved}</dd><dt>Provider invoice</dt><dd>{data.invoice}</dd></dl>
      <button type="button" disabled={data.checking}>Check Stop receipt</button>
    </section>}/>;
}
