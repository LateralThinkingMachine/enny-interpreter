# Rust/UI components

The operator and portable web interfaces use the Leptos edition of Rust/UI.
The local Card, CardHeader, CardContent and Button are adapted from the MIT-licensed registry at commit
`9cb8e9f251a254bea19c5d00318e296bb13dca05`. This is the Leptos submodule
of `rust-ui/ui` at `bdb1200276fca3e8d579685a2173f79a8ff1dd90`.

Card uses the upstream registry source and its class-merging macro, retaining the default size and the header/content parts used by these screens. We compile the pinned Rust/UI tw_merge implementation. Button uses the native button branch of the registry component with the interpreter stylesheet and stable Leptos attribute and event forwarding. No runtime component registry or hosted stylesheet is required.

[Card source](https://github.com/rust-ui/leptos-ui/blob/9cb8e9f251a254bea19c5d00318e296bb13dca05/app_crates/registry/src/ui/card.rs)
· [Button source](https://github.com/rust-ui/leptos-ui/blob/9cb8e9f251a254bea19c5d00318e296bb13dca05/app_crates/registry/src/ui/button.rs)
· [Upstream MIT license](LICENSE)

## License supplements

Several locked crates declare their license in Cargo metadata and omit the
license file from the published archive. The build uses these upstream texts
when assembling the offline redistribution notices.

- [Leptos](https://github.com/leptos-rs/leptos/blob/8298e2bce93183cb698adfd42fba06848817b80e/LICENSE)
- [Rust/UI tw_merge](https://github.com/rust-ui/ui/blob/eb39bc231835f06ae0ed9ee271d66d236c6141a8/crates/tw_merge/LICENSE)
- [rstml](https://github.com/rs-tml/rstml/blob/9fe0f4616a8224d37d64a53fa91722a08d736952/LICENSE)

The notice generator records versions, declared authors and license choices
from the locked manifests. It includes standard-library redistribution notices
from the pinned compiler and preserves duplicate texts through content hashes.
