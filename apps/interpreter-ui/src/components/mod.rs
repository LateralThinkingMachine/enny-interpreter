//! Adapted MIT-licensed components from the Rust/UI Leptos registry.
//! See the adjacent LICENSE and UPSTREAM.md.

// The stable class-merging macro from the upstream leptos_ui registry.
#[cfg(feature = "portable")]
macro_rules! clx {
    ($name:ident, $element:ident, $($base_class:expr),+ $(,)?) => {
        #[component]
        pub fn $name(
            #[prop(into, optional)] class: String,
            children: Children,
        ) -> impl IntoView {
            let merged_classes = tw_merge::tw_merge!(tw_merge::tw_join!($($base_class),+), class);

            view! {
                <$element
                    class=merged_classes
                    data-name=stringify!($name)
                >
                    {children()}
                </$element>
            }
        }
    };
}

pub mod button;
pub mod card;
