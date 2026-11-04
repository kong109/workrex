// Content links stay root-relative (`/guide/configuration/`) so pages read the
// same as upstream's; this prefixes the site's `base` onto them at build time.
export default function baseLinks({ base }) {
  const prefix = base.replace(/\/$/, "");

  const withBase = (url) =>
    typeof url === "string" &&
    url.startsWith("/") &&
    !url.startsWith("//") &&
    url !== prefix &&
    !url.startsWith(`${prefix}/`)
      ? prefix + url
      : url;

  const visit = (node) => {
    if (node.type === "link" || node.type === "definition") {
      node.url = withBase(node.url);
    } else if (
      node.type === "mdxJsxFlowElement" ||
      node.type === "mdxJsxTextElement"
    ) {
      for (const attr of node.attributes ?? []) {
        if (
          attr.type === "mdxJsxAttribute" &&
          (attr.name === "href" || attr.name === "src")
        ) {
          attr.value = withBase(attr.value);
        }
      }
    }
    node.children?.forEach(visit);
  };

  return () => (tree) => visit(tree);
}
