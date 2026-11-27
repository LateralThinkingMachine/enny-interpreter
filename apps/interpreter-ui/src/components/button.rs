//! The native-button rendering of Rust/UI's Button registry component.
//! See UPSTREAM.md and LICENSE.

use leptos::prelude::*;

#[component]
pub fn Button(#[prop(optional, into)] class: String, children: Children) -> impl IntoView {
    view! { <button type="button" data-name="Button" class=class>{children()}</button> }
}
