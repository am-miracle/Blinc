# CuteGirl G1 — untracked local-only asset

This directory is **intentionally untracked** (see
`Blinc/.gitignore`) because the source asset's license does not
permit commercial redistribution. Blinc's open-source repo sticks
to CC-BY-4.0 or more permissive so that downstream consumers can
embed demos in commercial products without stepping on a license
landmine.

The companion demo gracefully degrades when this directory is
missing: the viewport renders the loading overlay and logs a
polite "asset not present" error instead of panicking. If you want
to exercise the Phase 2 morph-target GPU path locally, grab the
asset as described below.

## Source & license

- **Model**: CuteGirl G1 by `tcube`
- **URL**: <https://sketchfab.com/3d-models/cutegirl-g1-4420eff446f047d89705091803bf3e8b>
- **License**: [CC-BY-NC-SA-4.0](http://creativecommons.org/licenses/by-nc-sa/4.0/)
  - Attribution required.
  - **No commercial use.** Distributing Blinc binaries that bundle
    this model is not permitted without a separate license.
  - Modifications / derivatives must carry the same license.

## Installing locally

1. Sign in to Sketchfab and visit the model page above.
2. Download the glTF 2.0 variant (`cutegirl-g1.zip` or similar).
3. Extract and drop the contents into this directory so the
   paths line up:

   ```
   examples/blinc_app_examples/examples/assets/3d/cutegirl_g1/
   ├── license.txt
   ├── scene.bin
   ├── scene.gltf
   └── textures/
   ```

4. Run the morph demo:

   ```sh
   cargo run -p blinc_app_examples --example cutegirl_morph_demo \
       --features windowed --release
   ```

## Attribution

If you publish screenshots, video, or derivative work using this
asset, include the following credit:

> This work is based on "CuteGirl G1"
> (<https://sketchfab.com/3d-models/cutegirl-g1-4420eff446f047d89705091803bf3e8b>)
> by tcube (<https://sketchfab.com/tcube>) licensed under CC-BY-NC-SA-4.0
> (<http://creativecommons.org/licenses/by-nc-sa/4.0/>).
