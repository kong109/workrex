# workrex documentation

The published site lives at
[kong109.github.io/workrex](https://kong109.github.io/workrex/). It is served
under the `/workrex` base path. Write internal links root-relative
(`/guide/configuration/`). The `plugins/base-links.mjs` remark plugin adds the
base to Markdown and MDX links at build time.

```bash
bun install
bun run dev
```

Run `bun run check`, `bun run format:check`, and `bun run build` before
submitting documentation changes.

To change the social preview image, edit `scripts/social-preview.svg` and run
`bun scripts/social-preview.mjs`.
