# Third-party notices

The standalone interpreter's first-party code is available under [Apache-2.0](LICENSE).

The source tree includes a patched copy of `libfvad-sys` 1.0.0, the Rust bindings for the WebRTC voice activity detector. Its upstream authors distribute the bindings under the [MIT](crates/adapters/vendor/libfvad-sys/LICENSE_MIT) or [Apache-2.0](crates/adapters/vendor/libfvad-sys/LICENSE_APACHE) license. Local patches update the build dependencies and binding generation, bound the shift in the rolling-minimum detector, and add an expiry regression. [Upstream provenance and file hashes](crates/adapters/vendor/libfvad-sys/UPSTREAM.json)

The bundled C library carries its own [BSD license](crates/adapters/vendor/libfvad-sys/src/libfvad/LICENSE), [patent grant](crates/adapters/vendor/libfvad-sys/src/libfvad/PATENTS) and [author credits](crates/adapters/vendor/libfvad-sys/src/libfvad/AUTHORS).

The four upstream audio files listed in the provenance manifest are omitted from this source distribution. They belong to the upstream C test suite. The interpreter tests generate their own audio input in memory.

The interpreter interfaces use the MIT-licensed Rust/UI registry's Button and Card components adapted for Leptos. The desktop downloads include compiled UI dependency notices in `notices/interpreter-ui.txt`. The portable download embeds its notices in the HTML file. [Component revisions and upstream license](apps/interpreter-ui/src/components/UPSTREAM.md)

Cargo and pnpm install the other dependencies under their respective licenses.

The packaged Mac app includes redistribution notices for its Rust and web dependencies and the Rust standard library under its Resources directory. The Linux archives carry the corresponding notices alongside the launcher. Browser-managed speech and translation models are obtained by the user's browser when needed.
