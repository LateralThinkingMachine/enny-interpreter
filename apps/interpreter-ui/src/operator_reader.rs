//! Local caption presentation. Only bounded display strings enter this surface.
use super::{State, flag, text};
use crate::components::button::Button;
use js_sys::Function;
use leptos::prelude::*;

pub(super) fn reader(state: State, _dispatch: Function) -> AnyView {
    // The host remounts this owner for a different session, input or interpretation.
    // Capture both lanes and their labels at one instant. Incoming text keeps
    // updating the normal state while a reader holds this one display snapshot.
    let (held, set_held) = signal(None::<serde_json::Value>);
    let (size, set_size) = signal("standard");
    let field = move |name: &'static str| {
        held.with(|snapshot| match snapshot {
            Some(snapshot) => snapshot[name].as_str().unwrap_or_default().to_owned(),
            None => text(state, name),
        })
    };
    let prefix = text(state, "prefix");
    let article = |original: bool| {
        let (id, heading, label, content, note) = if original {
            (
                "caption-preview",
                "original-heading",
                "sourceLabel",
                "sourceText",
                "sourceNote",
            )
        } else {
            (
                "interpretation-preview",
                "translated-heading",
                "targetLabel",
                "targetText",
                "targetNote",
            )
        };
        let heading = format!("{prefix}-{heading}");
        let labelled_by = heading.clone();
        view! {
            <article id=format!("{prefix}-{id}") aria-labelledby=labelled_by>
                <div class="meet-preview-heading"><h2 id=heading>{move || if original {field("sourceTitle")} else {"Interpretation".into()}}</h2><span>{move || field(label)}</span></div>
                <p class="meet-preview-text" dir="auto">{move || field(content)}</p>
                <p class="meet-hint">{move || field(note)}</p>
            </article>
        }
    };
    view! {
        <section class="caption-reader" aria-label="Caption reader" data-size=move || size.get()>
            <div class="reader-controls">
                <Button attr:disabled=move || held.with(Option::is_none) && !flag(state, "hasWords")
                    on:click=move |_| {
                        if held.with_untracked(Option::is_some) {set_held.set(None);}
                        else if state.with_untracked(|s| s["hasWords"] == true) {set_held.set(Some(state.get_untracked()));}
                    }>
                    {move || if held.with(Option::is_some) {"Return to live text"} else {"Hold text"}}
                </Button>
                <label>"Caption size"
                    <select prop:value=move || size.get() on:change=move |event| {
                        let next = match event_target_value(&event).as_str() {
                            "standard" => "standard", "large" => "large", "extra" => "extra", _ => return,
                        };
                        set_size.set(next);
                    }>
                        <option value="standard">"Standard"</option><option value="large">"Large"</option>
                        <option value="extra">"Extra large"</option>
                    </select>
                </label>
                <span role="status" aria-live="polite">{move || if held.with(Option::is_some) {"Text held on this screen."} else {"Showing the latest available text."}}</span>
            </div>
            <div class="meet-text-preview" aria-live="off">{article(true)}{article(false)}</div>
        </section>
    }.into_any()
}
