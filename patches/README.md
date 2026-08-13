# Vendored-crate patches

`vendor/` is gitignored, so any edit made there is invisible to git and is silently
reverted by a re-vendor. Every such edit must have a patch file here.

## Re-vendor procedure

After replacing `vendor/bevy_vrm1` with a fresh copy:

```bash
git apply -p0 patches/bevy_vrm1-0.9.0-outline-doublesided.patch
```

`-p0` because the patch paths are already repo-root-relative. Verify from the repo
root; `git apply -p0 --reverse --check <patch>` succeeding means the patch is
currently applied. Re-check whenever the crate version moves.

## bevy_vrm1-0.9.0-outline-doublesided.patch

Deletes the `cull_mode: None` gate in `queue_outlines`
(`vendor/bevy_vrm1/src/vrm/mtoon/outline_pass.rs`).

Upstream skips the outline pass for double-sided materials, reasoning that an
inverted hull on a double-sided mesh overwrites correctly-rendered pixels. That
does not apply to this crate's own implementation: `outline_pass/pipeline.rs`
calls `descriptor.primitive.cull_mode.replace(Face::Front)` after `specialize`,
so the hull is back-faces-only whatever the material's main-pass cull mode is.

VRMs whose materials are all `doubleSided: true` hit this: the outline pass
submits zero geometry and no MToon outline can ever be drawn — no config value
changes that.

Per-material opt-in is unaffected: `mtoon_fragment.wgsl` still discards outline
fragments whose mode is not `OUTLINE_WORLD_COORDINATES`, which is driven by
`outline_mode` in `config/ModelOverrides/<model>/mtoon_overrides.json`.

Do **not** instead add a `cull_mode` / `double_sided` key to `MToonOverrideEntry`.
`MToonMaterialKey` feeds the same field to the *main* pass, so forcing
`Some(Face::Back)` turns those meshes single-sided everywhere and punches
see-through holes in every open shell (skirts, collars, clothes, trims).
