# bloq_editor

The editor is a standalone native
and WASM binary; see [installation](../docs/getting-started/installation.md#editor-application) for platform requirements.

- Run `rtk just test bloq_editor` and `rtk just fmt-check`. Check the WASM build
  when changing target-dependent code; browser worker smoke instructions are in
  `tests/browser_worker_smoke.htm`.
- Check WASM with `cargo check -p bloq_editor --target wasm32-unknown-unknown
  --locked`. Adding `--all-features` fails: `hotpath` needs native thread CPU
  metrics, and the web build leaves both hotpath features off.
- Keep persistent egui state under stable IDs. Key item state by domain identity;
  use position only for slot state. Stateless widgets need no explicit ID.
- `warn_if_rect_changes_id` can produce false positives. Test the affected state,
  not the diagnostic painting.
