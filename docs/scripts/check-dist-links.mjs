// Checks that every internal link and asset in the built site resolves under
// the site's base path. Run after `bun run build`:
//   bun scripts/check-dist-links.mjs
import fs from "node:fs";
import path from "node:path";

const base = "/workrex/";
const origin = "https://kong109.github.io";
const dist = new URL("../dist/", import.meta.url).pathname;

function walk(directory) {
  return fs.readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const location = path.join(directory, entry.name);
    return entry.isDirectory() ? walk(location) : [location];
  });
}

const files = walk(dist);
const pages = files.filter((file) => file.endsWith(".html"));
const styles = files.filter((file) => file.endsWith(".css"));
const ids = new Map();

function idsOf(file) {
  if (!ids.has(file)) {
    const html = fs.readFileSync(file, "utf8");
    ids.set(
      file,
      new Set([...html.matchAll(/\sid="([^"]+)"/g)].map((m) => m[1])),
    );
  }
  return ids.get(file);
}

function target(pathname) {
  const local = path.join(
    dist,
    decodeURIComponent(pathname.slice(base.length)),
  );
  if (fs.existsSync(local) && fs.statSync(local).isFile()) return local;
  const index = path.join(local, "index.html");
  return fs.existsSync(index) ? index : null;
}

const failures = [];
let checked = 0;

function check(file, ref) {
  if (ref.startsWith("//")) return;
  if (/^[a-z][a-z0-9+.-]*:/i.test(ref) && !ref.startsWith(`${origin}/`)) return;
  const url = new URL(ref, origin + base + path.relative(dist, file));
  if (url.origin !== origin) return;
  checked++;
  const where = path.relative(dist, file);
  // Starlight gives its 404 page a /404/ canonical URL; Pages serves 404.html.
  if (where === "404.html" && url.pathname === `${base}404/`) return;
  if (!url.pathname.startsWith(base)) {
    failures.push(`${where}: ${ref} is outside ${base}`);
    return;
  }
  const resolved = target(url.pathname);
  if (!resolved) {
    failures.push(`${where}: ${ref} does not resolve`);
    return;
  }
  const hash = decodeURIComponent(url.hash.slice(1));
  if (hash && resolved.endsWith(".html") && !idsOf(resolved).has(hash)) {
    failures.push(`${where}: ${ref} has no #${hash}`);
  }
}

for (const file of pages) {
  const html = fs.readFileSync(file, "utf8");
  for (const [, , ref] of html.matchAll(/\s(href|src|content)="([^"]*)"/g)) {
    if (ref.startsWith("/") || ref.startsWith(origin) || ref.startsWith("#")) {
      check(file, ref.replaceAll("&amp;", "&"));
    }
  }
  for (const [, set] of html.matchAll(/\ssrcset="([^"]*)"/g)) {
    for (const candidate of set.split(",")) {
      check(file, candidate.trim().split(/\s+/)[0]);
    }
  }
}

for (const file of styles) {
  const css = fs.readFileSync(file, "utf8");
  for (const [, ref] of css.matchAll(/url\(\s*["']?(\/[^"')]+)["']?\s*\)/g)) {
    check(file, ref);
  }
}

// The Copy page Markdown is served raw, so its links must be absolute site URLs.
for (const file of files.filter((f) => f.endsWith(".md"))) {
  const markdown = fs.readFileSync(file, "utf8");
  const refs = [
    ...[...markdown.matchAll(/\]\(([^)\s]+)/g)].map((m) => m[1]),
    ...[...markdown.matchAll(/^\s*\[[^\]]+\]:\s*(\S+)/gm)].map((m) => m[1]),
  ];
  for (const ref of refs) {
    if (ref.startsWith("/") && !ref.startsWith("//")) {
      failures.push(`${path.relative(dist, file)}: ${ref} is root-relative`);
    } else if (ref.startsWith(`${origin}/`)) {
      check(file, ref);
    }
  }
}

console.log(`${pages.length} pages, ${checked} internal references checked`);
if (failures.length > 0) {
  console.error(failures.join("\n"));
  console.error(`${failures.length} broken references`);
  process.exit(1);
}
