// Renders public/social-preview.png from scripts/social-preview.svg:
//   bun scripts/social-preview.mjs
import sharp from "sharp";

const source = new URL("./social-preview.svg", import.meta.url).pathname;
const output = new URL("../public/social-preview.png", import.meta.url)
  .pathname;

await sharp(source).png({ compressionLevel: 9 }).toFile(output);
