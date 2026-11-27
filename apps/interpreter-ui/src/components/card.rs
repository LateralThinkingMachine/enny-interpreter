//! Rust/UI Card registry component. See UPSTREAM.md and LICENSE.

use leptos::prelude::*;

#[cfg(feature = "portable")]
mod components {
    use super::*;

    clx! {CardHeader, div, "@container/card-header flex flex-col items-start gap-1.5 px-6 [[data-size=sm]_&]:px-4 [.border-b]:pb-6 sm:grid sm:auto-rows-min sm:grid-rows-[auto_auto] has-data-[slot=card-action]:sm:grid-cols-[1fr_auto]"}
    clx! {CardContent, div, "px-6 [[data-size=sm]_&]:px-4"}
}

#[component]
pub fn Card(#[prop(into, optional)] class: String, children: Children) -> impl IntoView {
    let merged = tw_merge::tw_merge!(
        "bg-card text-card-foreground flex flex-col rounded-xl border shadow-sm",
        "py-6 gap-4",
        class
    );

    view! {
        <div class=merged data-size="default" data-name="Card">
            {children()}
        </div>
    }
}

#[cfg(feature = "portable")]
pub use components::{CardContent, CardHeader};
