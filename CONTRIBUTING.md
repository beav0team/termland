# Contributing to termland

termland is alpha (`v0.1`, host tier on kitty). The goal is a Linux distribution front end — one process: compositor + shell + terminal — not a distro in year one. See `docs/DISTRO.md` for the roadmap.

## Submitting changes

1. Fork `beav0team/termland` and create a branch from `main`.
2. Make your change, add or update tests, and run:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo build --release
```

3. Open a pull request against `main`. Use the PR template. Keep diffs small and focused.

## Style

- Follow the existing code style (match surrounding files; `rustfmt` is enforced in CI).
- Public items need doc comments.
- UI geometry changes need a regression test where possible (see `ui.rs`, `files.rs`, `desktop.rs` tests).

## License

MIT. By contributing you agree your contributions are licensed under MIT.
