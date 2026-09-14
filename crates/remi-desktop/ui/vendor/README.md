# Vendored runtime

No build step (plan §0 #4), so third-party JS is committed here rather than installed.

| file | what | provenance |
|---|---|---|
| `spine-webgl.js` | Esoteric Software's official Spine WebGL runtime, IIFE build — defines the global `spine` | `@esotericsoftware/spine-webgl@4.2.120`, `dist/iife/spine-webgl.min.js`, fetched from unpkg 2026-09-14 |

`sha256(spine-webgl.js) = 9e495034588d2195c379422df37151a0750f79e2f4211434ca482520da3df6e0`

**The major/minor must stay `4.2`.** The asset was exported from Spine 4.2.43 and uses physics
constraints, which are a 4.2 feature: a 4.1 runtime rejects the file outright, and a 4.3 runtime
is a different skeleton format again. `4.2-latest` is the tag to track.

To update:

```sh
curl -sSL -o crates/remi-desktop/ui/vendor/spine-webgl.js \
  https://unpkg.com/@esotericsoftware/spine-webgl@4.2.120/dist/iife/spine-webgl.min.js
```
