use super::{Actions, State, flag, text};
use js_sys::Function;
use leptos::prelude::*;

pub(super) fn speech_settings(state: State, dispatch: Function) -> AnyView {
    let actions = Actions(StoredValue::new_local(dispatch));
    view! {
        <details class="speech-settings">
            <summary>"Caption accuracy "<span>"Optional"</span></summary>
            <fieldset disabled=move || flag(state, "disabled")>
                <legend class="sr-only">"Session caption hints"</legend>
                <Show when=move || !flag(state, "pair") fallback=|| view! {
                    <p class="meet-hint">"Caption hints follow the selected language pair, or Japanese, English, Hindi, Spanish and Vietnamese in Auto mode. Hints guide recognition and do not identify the spoken language."</p>
                }>
                    <label for="spoken-language">"Expected spoken language"
                        <select id="spoken-language" prop:value=move || text(state, "language")
                            on:change=move |event| actions.emit("language", &event_target_value(&event))>
                            <option value="auto">"Japanese, English and Hindi"</option>
                            <option value="ja">"Japanese · 日本語"</option>
                            <option value="en">"English"</option>
                            <option value="hi">"Hindi · हिन्दी"</option>
                        </select>
                    </label>
                    <p class="meet-hint">"Keep all three for mixed-language speech. This is independent of the interpretation output language."</p>
                </Show>
                <Show when=move || !flag(state, "noKeywords")>
                    <label for="speech-terms">"Names and specialist terms"
                        <textarea id="speech-terms" rows="3" maxlength="4096" spellcheck="false" autocomplete="off"
                            aria-describedby="speech-terms-help speech-terms-error"
                            aria-invalid=move || (!text(state, "error").is_empty()).to_string()
                            placeholder="One name, acronym or product term per line"
                            prop:value=move || text(state, "terms")
                            on:input=move |event| actions.emit("terms", &event_target_value(&event))></textarea>
                    </label>
                    <p id="speech-terms-help" class="meet-hint">"Up to 20 terms. Sent to the caption service only when you start; kept in memory for this session. Hints may help recognition but do not guarantee exact words."</p>
                </Show>
            </fieldset>
        </details>
        <p id="speech-terms-error" class="meet-hint" role="status">{move || {
            let message = text(state, "error");
            if message.is_empty() { message } else { format!("{message} Open Caption accuracy to correct or clear the optional terms.") }
        }}</p>
    }.into_any()
}
