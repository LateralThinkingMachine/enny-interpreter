//! Form controls shared by the optional listening, route and budget panels.
use super::{Actions, State, flag, text};
use js_sys::Function;
use leptos::prelude::*;

fn attr(state: State, key: &str) -> Option<String> {
    state.with(|s| match &s[key] {
        serde_json::Value::String(value) => Some(value.clone()),
        serde_json::Value::Number(value) => Some(value.to_string()),
        serde_json::Value::Bool(value) => Some(value.to_string()),
        _ => None,
    })
}

pub(super) fn select(state: State, dispatch: Function) -> AnyView {
    let actions = Actions(StoredValue::new_local(dispatch));
    view! {
        <select id=move || attr(state, "id") disabled=move || flag(state, "disabled")
            aria-describedby=move || attr(state, "aria-describedby")
            prop:value=move || text(state, "value") on:change=move |ev| actions.emit("change", &event_target_value(&ev))>
            {move || state.with(|s| s["options"].as_array().into_iter().flatten().map(|item| {
                let value = item["value"].as_str().unwrap_or_default();
                let selected = s["value"].as_str() == Some(value);
                let disabled = item["disabled"].as_bool().unwrap_or(false);
                let label = item["label"].as_str().unwrap_or_default().to_owned();
                view! { <option value=value.to_owned() selected=selected disabled=disabled>{label}</option> }
            }).collect_view())}
        </select>
    }.into_any()
}

pub(super) fn textarea(state: State, dispatch: Function) -> AnyView {
    let actions = Actions(StoredValue::new_local(dispatch));
    view! {
        <textarea id=move || attr(state, "id") rows=move || attr(state, "rows")
            maxlength=move || attr(state, "maxLength") placeholder=move || attr(state, "placeholder")
            aria-invalid=move || attr(state, "aria-invalid") aria-describedby=move || attr(state, "aria-describedby")
            prop:value=move || text(state, "value")
            on:input=move |ev| actions.emit("change", &event_target_value(&ev))></textarea>
    }.into_any()
}

pub(super) fn input(state: State, dispatch: Function) -> AnyView {
    let actions = Actions(StoredValue::new_local(dispatch));
    let checked_input =
        state.with_untracked(|s| matches!(s["type"].as_str(), Some("checkbox" | "radio")));
    view! {
        <input type=move || text(state, "type") id=move || attr(state, "id") name=move || attr(state, "name")
            min=move || attr(state, "min") max=move || attr(state, "max") step=move || attr(state, "step")
            disabled=move || flag(state, "disabled") aria-invalid=move || attr(state, "aria-invalid")
            prop:checked=move || flag(state, "checked")
            prop:value=move || attr(state, "value").unwrap_or_else(|| if checked_input { "on".to_owned() } else { String::new() })
            on:input=move |ev| {
                if !checked_input { actions.emit("change", &event_target_value(&ev)); }
            }
            on:change=move |ev| {
                if checked_input { actions.emit("change", if event_target_checked(&ev) { "true" } else { "false" }); }
            }/>
    }.into_any()
}
