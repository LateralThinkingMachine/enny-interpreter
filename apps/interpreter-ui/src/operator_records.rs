use super::{Actions, State, flag, text};
use crate::components::{button::Button, card::Card};
use js_sys::Function;
use leptos::prelude::*;

pub(super) fn session(state: State, dispatch: Function) -> AnyView {
    let actions = Actions(StoredValue::new_local(dispatch));
    view! {
        <header class="topbar">
            <a class="brand" href="#main"><span class="brand-mark" aria-hidden="true">"E"</span>
                <span>"Enny"<small>{move || if flag(state, "operator") { "Private operator" } else { "Approved public screen" }}</small></span>
            </a>
            <div class="header-actions"><span class="pilot-badge">"Supervised pilot · unverified"</span>
                <Show when=move || !flag(state, "stopped")>
                    <Button class="stop-button" on:click=move |_| actions.emit("stop", "")>
                        {move || if flag(state, "operator") { "■ Stop pilot" } else { "■ Stop public screen" }}
                    </Button>
                </Show>
                <Show when=move || flag(state, "stopped") && !flag(state, "stopConfirmed")>
                    <Button class="quiet" on:click=move |_| actions.emit("stop", "")>"Retry server Stop"</Button>
                </Show>
                <Show when=move || flag(state, "authenticated")>
                    <Button class="quiet" on:click=move |_| actions.emit("logout", "")>"Sign out"</Button>
                </Show>
            </div>
        </header>
    }.into_any()
}

pub(super) fn notes(state: State, dispatch: Function) -> AnyView {
    let actions = Actions(StoredValue::new_local(dispatch));
    view! {
        <Card class="panel"><div class="section-heading"><div><p class="eyebrow">"Your own draft"</p>
            <h2>"Private notes"</h2></div>
            <div class="button-row">
                <Button class="quiet" on:click=move |_| actions.emit("export", "")>"Export private captions & notes (.txt)"</Button>
                <Button attr:id="notes-finish-export" attr:disabled=move || flag(state, "finishDisabled")
                    on:click=move |_| actions.emit("finish", "")>{move || text(state, "finishLabel")}</Button>
            </div></div>
            <label for="private-notes">"Manual draft · not shared or automatically inferred"
                <textarea id="private-notes" maxlength="20000" rows="7"
                    placeholder="Write your own notes. Verify names, numbers, decisions and commitments."
                    prop:value=move || text(state, "notes")
                    on:input=move |event| actions.emit("notes", &event_target_value(&event))></textarea>
            </label>
            <p class="help">"Only this page’s memory holds these notes. Export deliberately creates a local file. Finish stops capture and sound immediately, waits up to 35 seconds for final source captions, then exports and stops the pilot. An incomplete finish is labeled in the file. Stop and signout cancel a pending finish and clear unsaved work."</p>
        </Card>
    }.into_any()
}

pub(super) fn stop_receipt(state: State, dispatch: Function) -> AnyView {
    let actions = Actions(StoredValue::new_local(dispatch));
    let rows = [
        ("Browser audio", "audio"),
        ("Private WebRTC audio", "rtc"),
        ("Server Stop", "ack"),
        ("Provider transport", "transport"),
        ("Local reservation", "reserved"),
        ("Provider invoice", "invoice"),
    ];
    view! {
        <section class="stop-receipt" aria-labelledby="stop-receipt-heading">
            <h2 id="stop-receipt-heading">"Stop receipt"</h2>
            <dl>{rows.into_iter().map(|(heading, key)| view! {<dt>{heading}</dt><dd>{move || text(state, key)}</dd>}).collect_view()}</dl>
            <Button class="quiet" attr:disabled=move || flag(state, "checking")
                on:click=move |_| actions.emit("check", "")>"Check Stop receipt"</Button>
        </section>
    }.into_any()
}

pub(super) fn viewer(state: State, dispatch: Function) -> AnyView {
    let actions = Actions(StoredValue::new_local(dispatch));
    view! {
        <Card class="panel presentation">
            <div class="section-heading"><div><p class="eyebrow">"Approved public screen"</p>
                <h1>{move || if flag(state, "hasDelivery") { "For the conversation." } else { "Ready when you approve." }}</h1></div>
                <span class="tag">{move || if flag(state, "connected") { "Viewer connected" } else { "Viewer closed" }}</span>
            </div>
            <p>"Share only this tab with audio in your meeting app. This screen receives no private captions, notes or Live audio. Confirm actual remote hearing on a second device before the call."</p>
            <Button class="arm-button" attr:disabled=move || flag(state, "connectDisabled")
                on:click=move |_| actions.emit("connect", "")>"Enable this public screen & sound"</Button>
            <Show when=move || flag(state, "hasDelivery") fallback=|| view! {
                <div class="public-empty"><span aria-hidden="true">"◌"</span><h2>"No approved statement yet."</h2>
                <p>"Reloading never replays a prior clip."</p></div>
            }>
                <article class="public-delivery" lang=move || text(state, "language")>
                    <p class="eyebrow">{move || format!("Approved wording · {}", text(state, "languageLabel"))}</p>
                    <div class="presentation-words">{move || text(state, "words")}</div>
                    <p class="receipt">{move || format!("{} · local browser report only", text(state, "receipt"))}</p>
                    <p class="help">"No replay control. A new statement requires a new operator preview and approval."</p>
                </article>
            </Show>
        </Card>
    }.into_any()
}

pub(super) fn fragments(state: State, _dispatch: Function) -> AnyView {
    let keys = move || {
        state.with(|s| {
            s["order"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|key| key.as_str().map(str::to_owned))
                .collect::<Vec<_>>()
        })
    };
    view! {
        <Show when=move || state.with(|s| s["order"].as_array().is_some_and(|order| !order.is_empty()))
            fallback=move || view! {<div class="empty-state"><span aria-hidden="true">"◌"</span><p>{move || text(state, "empty")}</p></div>}>
            <ol class="fragment-list" aria-label=move || text(state, "label")>
                <For each=keys key=|key| key.clone() children=move |key| {
                    let id = StoredValue::new(key);
                    let field = move |name: &str| state.with(|s| id.with_value(|key| {
                        s["rows"][key][name].as_str().unwrap_or_default().to_owned()
                    }));
                    let fixed = move |name: &str| state.with(|s| id.with_value(|key| {
                        s["rows"][key]["fixed"][name].as_str().map(str::to_owned)
                    }));
                    view! {
                        <li data-interpretation-target=move || fixed("language")
                            data-interpretation-source=move || fixed("source")
                            data-interpretation-mode=move || fixed("language").map(|_| "fixed")>
                            <div class="fragment-label"><span>{move || field("source")}</span><span>{move || field("status")}</span></div>
                            <p>{move || field("text")}</p>
                            <Show when=move || !field("note").is_empty()><small>{move || field("note")}</small></Show>
                        </li>
                    }
                }/>
            </ol>
        </Show>
    }.into_any()
}
