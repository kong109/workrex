import type { APIRoute, GetStaticPaths } from "astro";
import { getCollection, type CollectionEntry } from "astro:content";

function rawSlug(id: string): string {
  return id.endsWith("/index") ? id.slice(0, -"/index".length) : id;
}

// The raw body skips the base-links remark plugin, so make links absolute here.
const siteRoot = new URL(
  import.meta.env.BASE_URL,
  import.meta.env.SITE,
).href.replace(/\/$/, "");

function readableBody(entry: CollectionEntry<"docs">): string {
  return (entry.body ?? "")
    .replace(/^import .*;\s*$/gm, "")
    .replace(/<Tabs(?:\s[^>]*)?>/g, "")
    .replace(/<\/Tabs>/g, "")
    .replace(/\s*<TabItem label="([^"]+)">\s*/g, "\n\n### $1\n\n")
    .replace(/\s*<\/TabItem>\s*/g, "\n")
    .replace(/<Screenshot[\s\S]*?alt="([^"]+)"[\s\S]*?\/>/g, "_Screenshot: $1_")
    .replace(
      /<RemoteDemo[\s\S]*?caption="([^"]+)"[\s\S]*?\/>/g,
      "_Animation: $1_",
    )
    .replace(/\]\(\/(?!\/)/g, `](${siteRoot}/`)
    .replace(/^(\s*\[[^\]]+\]:\s*)\/(?!\/)/gm, `$1${siteRoot}/`)
    .trim();
}

export const getStaticPaths = (async () => {
  const entries = await getCollection("docs", ({ data }) => !data.draft);
  return entries.map((entry) => ({
    params: { slug: rawSlug(entry.id) },
    props: { entry },
  }));
}) satisfies GetStaticPaths;

export const GET: APIRoute = ({ props }) => {
  const entry = props.entry as CollectionEntry<"docs">;
  const markdown = `# ${entry.data.title}\n\n${readableBody(entry)}\n`;

  return new Response(markdown, {
    headers: { "Content-Type": "text/markdown; charset=utf-8" },
  });
};
