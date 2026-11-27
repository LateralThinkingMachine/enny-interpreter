use super::{Actions, State, flag, text};
use crate::components::button::Button;
use js_sys::Function;
use leptos::prelude::*;

pub(super) fn setup(state: State, dispatch: Function) -> AnyView {
    let actions = Actions(StoredValue::new_local(dispatch));
    let options =
        move || {
            state.with(|s| {
                s["languages"].as_array().into_iter().flatten().map(|item| {
                let id = item["value"].as_str().unwrap_or_default().to_owned();
                let label = item["label"].as_str().unwrap_or_default().to_owned();
                let selected = s["language"].as_str() == Some(&id);
                let disabled = item["disabled"].as_bool().unwrap_or(false);
                view! { <option value=id selected=selected disabled=disabled>{label}</option> }
            }).collect_view()
            })
        };
    view! {
        <div class="meet-start-row">
            <label for="meet-source">"Listen to"
                <select id="meet-source" aria-describedby="meet-source-help"
                    prop:value=move || text(state, "source") disabled=move || flag(state, "sourceDisabled")
                    on:change=move |event| actions.emit("source", &event_target_value(&event))>
                    <option value="remote_mix">"Meeting tab · other people"</option>
                    <option value="self">"My microphone · my voice"</option>
                </select>
            </label>
            <Show when=move || flag(state, "showLanguage")>
                <label for="meet-language">{move || text(state, "languageLabel")}
                    <select id="meet-language" prop:value=move || text(state, "language")
                        disabled=move || flag(state, "languageDisabled")
                        on:change=move |event| actions.emit("language", &event_target_value(&event))>
                        {options}
                    </select>
                </label>
            </Show>
            <Show when=move || !flag(state, "rehearsal")>
            <Button class="primary-button" attr:id="start-meet"
                attr:aria-describedby=move || (!flag(state, "paused")).then_some("meet-start-estimate meet-start-agreement meet-start-help")
                attr:disabled=move || !flag(state, "canStart")
                on:click=move |_| actions.emit("start", "")>
                {move || text(state, "startLabel")}
            </Button>
            </Show>
        </div>
    }.into_any()
}

pub(super) fn audio_check(state: State, dispatch: Function) -> AnyView {
    let actions = Actions(StoredValue::new_local(dispatch));
    view! {
        <label class="check meet-consent">
            <input id="capture-agreement" type="checkbox" prop:checked=move || flag(state, "consent")
                disabled=move || flag(state, "finishing")
                on:change=move |event| actions.emit("consent", if event_target_checked(&event) {"true"} else {"false"})/>
            <span>{move || text(state, "consentLabel")}</span>
        </label>
        <div class="button-row">
            <Button attr:class=move || if flag(state, "rehearsal") { "primary-button" } else { "" } attr:id="choose-meet-tab" attr:aria-describedby=move || flag(state, "rehearsal").then_some("meet-start-estimate") attr:disabled=move || !flag(state, "available") || !flag(state, "consent")
                on:click=move |_| actions.emit("check", "")>{move || text(state, "label")}</Button>
        </div>
    }.into_any()
}

pub(super) fn transport(state: State, dispatch: Function) -> AnyView {
    let actions = Actions(StoredValue::new_local(dispatch));
    view! {
        <Show when=move || flag(state, "pause")>
            <Button attr:id="pause-interpretation" on:click=move |_| actions.emit("pause", "")>"Pause interpretation"</Button>
        </Show>
        <Button class="stop-button" attr:id="stop-meet" on:click=move |_| actions.emit("stop", "")>"Stop"</Button>
    }.into_any()
}

pub(super) fn sound(state: State, dispatch: Function) -> AnyView {
    let actions = Actions(StoredValue::new_local(dispatch));
    view! {
        <div class="meet-sound-control"><div>
            <strong>{move || format!("Interpretation · {}", text(state, "language"))}</strong>
            <p id="meet-sound-help">"Private headphone sound. Muting discards sound; processing and reservations continue. Enabling sound never replays the muted interval."</p>
        </div>
        <Button attr:id="meet-sound-toggle" attr:aria-pressed=move || (!flag(state, "muted")).to_string()
            attr:aria-describedby="meet-sound-help" attr:disabled=move || flag(state, "disabled")
            on:click=move |_| actions.emit("mute", "")>
            {move || if flag(state, "muted") { "Enable interpretation sound" } else { "Mute interpretation sound" }}
        </Button></div>
    }.into_any()
}

/// Form submission stays native. A scoped callback only handles explicit clicks.
pub(super) fn button(state: State, dispatch: Function) -> AnyView {
    let actions = Actions(StoredValue::new_local(dispatch));
    let optional = move |key: &str| state.with(|s| s[key].as_str().map(str::to_owned));
    let aria = move |key: &str| state.with(|s| s[key].as_bool().map(|v| v.to_string()));
    // A panel's class is structural; the remaining attributes can change in place.
    let class = state.with_untracked(|s| s["className"].as_str().unwrap_or_default().to_owned());
    view! {
        <Button class=class attr:id=move || optional("id")
            attr:r#type=move || if text(state, "type") == "submit" { "submit" } else { "button" }
            attr:disabled=move || flag(state, "disabled")
            attr:aria-describedby=move || optional("aria-describedby")
            attr:aria-controls=move || optional("aria-controls")
            attr:aria-pressed=move || aria("aria-pressed")
            attr:aria-expanded=move || aria("aria-expanded")
            on:click=move |_| actions.emit("click", "")>{move || text(state, "label")}</Button>
    }
    .into_any()
}
