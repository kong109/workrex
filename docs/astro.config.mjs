// @ts-check
import { defineConfig } from "astro/config";
import starlight from "@astrojs/starlight";
import starlightLinksValidator from "starlight-links-validator";
import starlightLlmsTxt from "starlight-llms-txt";
import baseLinks from "./plugins/base-links.mjs";
import changelogReleases from "./plugins/changelog-releases.mjs";

const site = "https://kong109.github.io";
const base = "/workrex";
const repo = "https://github.com/kong109/workrex";
const socialImage = `${site}${base}/favicon.svg`;

export default defineConfig({
  site,
  base,
  trailingSlash: "always",
  build: { format: "directory" },
  redirects: {
    "/": `${base}/guide/`,
    "/guide/sandbox/remote/": `${base}/guide/remote/`,
  },
  markdown: { remarkPlugins: [changelogReleases, baseLinks({ base })] },
  image: {
    layout: "constrained",
    responsiveStyles: true,
  },
  integrations: [
    starlight({
      title: "workrex",
      description:
        "Run parallel AI coding agents in git worktrees, each in its own Rex session",
      locales: {
        root: { label: "English", lang: "en-US" },
      },
      logo: { src: "./src/assets/workrex-mark.svg", alt: "" },
      favicon: "/favicon.svg",
      lastUpdated: true,
      plugins: [
        starlightLlmsTxt(),
        starlightLinksValidator({
          errorOnRelativeLinks: true,
          errorOnInvalidHashes: true,
        }),
      ],
      editLink: {
        baseUrl: `${repo}/edit/main/docs/`,
      },
      social: [
        {
          icon: "github",
          label: "GitHub",
          href: repo,
        },
      ],
      components: {
        SocialIcons: "./src/components/HeaderLinks.astro",
        PageTitle: "./src/components/PageTitle.astro",
      },
      customCss: [
        "./src/styles/theme.css",
        "./src/styles/layout.css",
        "./src/styles/code.css",
      ],
      head: [
        {
          tag: "meta",
          attrs: { property: "og:image", content: socialImage },
        },
        {
          tag: "meta",
          attrs: { property: "og:image:width", content: "1280" },
        },
        {
          tag: "meta",
          attrs: { property: "og:image:height", content: "640" },
        },
        {
          tag: "meta",
          attrs: {
            property: "og:image:alt",
            content: "workrex: parallel AI agents in git worktrees, run in Rex",
          },
        },
        {
          tag: "meta",
          attrs: { name: "twitter:image", content: socialImage },
        },
        {
          tag: "script",
          attrs: { src: `${base}/image-zoom.js`, defer: true },
        },
      ],
      sidebar: [
        {
          label: "Getting Started",
          items: [
            { label: "What is workrex?", slug: "guide" },
            { label: "Installation", slug: "guide/installation" },
            { label: "Quick start", slug: "guide/quick-start" },
            { label: "Rex", slug: "guide/rex" },
            { label: "Configuration", slug: "guide/configuration" },
            { label: "Commands", slug: "reference/commands" },
          ],
        },
        {
          label: "AI Agents",
          items: [
            { label: "Overview", slug: "guide/agents" },
            { label: "Workflows", slug: "guide/workflows" },
            { label: "Claude Code", slug: "guide/claude-code" },
            { label: "Status tracking", slug: "guide/status-tracking" },
            { label: "Skills", slug: "guide/skills" },
          ],
        },
        {
          label: "Dashboard",
          items: [
            { label: "Overview", slug: "guide/dashboard" },
            { label: "Diff view", slug: "guide/dashboard/diff-view" },
            { label: "Patch mode", slug: "guide/dashboard/patch-mode" },
            { label: "Configuration", slug: "guide/dashboard/configuration" },
          ],
        },
        {
          label: "Sidebar",
          items: [
            { label: "Overview", slug: "guide/sidebar" },
            { label: "Customization", slug: "guide/sidebar/customization" },
          ],
        },
        {
          label: "Sandbox",
          items: [
            { label: "Overview", slug: "guide/sandbox" },
            { label: "Container backend", slug: "guide/sandbox/container" },
            { label: "Lima VM backend", slug: "guide/sandbox/lima" },
            { label: "Shared features", slug: "guide/sandbox/features" },
            { label: "Alternatives", slug: "guide/sandbox/alternatives" },
          ],
        },
        {
          label: "Remote servers",
          badge: { text: "New", variant: "tip" },
          items: [
            { label: "Overview", slug: "guide/remote" },
            { label: "Setting up a server", slug: "guide/remote/setup" },
            { label: "Logins and secrets", slug: "guide/remote/logins" },
            {
              label: "Working with remote agents",
              slug: "guide/remote/usage",
            },
            { label: "Bringing work home", slug: "guide/remote/git" },
            {
              label: "Maintenance and troubleshooting",
              slug: "guide/remote/troubleshooting",
            },
          ],
        },
        {
          label: "Guides",
          items: [
            { label: "Session mode", slug: "guide/session-mode" },
            { label: "direnv", slug: "guide/direnv" },
            { label: "Monorepos", slug: "guide/monorepos" },
            {
              label: "Git worktree caveats",
              slug: "guide/git-worktree-caveats",
            },
            { label: "Nix", slug: "guide/nix" },
            { label: "Changelog", slug: "changelog" },
          ],
        },
        {
          label: "Commands",
          items: [
            { label: "Overview", slug: "reference/commands" },
            { label: "add", slug: "reference/commands/add" },
            { label: "merge", slug: "reference/commands/merge" },
            { label: "rebase", slug: "reference/commands/rebase" },
            { label: "remove", slug: "reference/commands/remove" },
            { label: "rename", slug: "reference/commands/rename" },
            { label: "list", slug: "reference/commands/list" },
            { label: "status", slug: "reference/commands/status" },
            { label: "open", slug: "reference/commands/open" },
            { label: "close", slug: "reference/commands/close" },
            { label: "resurrect", slug: "reference/commands/resurrect" },
            { label: "sync-files", slug: "reference/commands/sync-files" },
            { label: "path", slug: "reference/commands/path" },
            { label: "shell", slug: "reference/commands/shell" },
            { label: "dashboard", slug: "reference/commands/dashboard" },
            { label: "sidebar", slug: "reference/commands/sidebar" },
            { label: "reap-agents", slug: "reference/commands/reap-agents" },
            { label: "send", slug: "reference/commands/send" },
            { label: "capture", slug: "reference/commands/capture" },
            { label: "wait", slug: "reference/commands/wait" },
            { label: "run", slug: "reference/commands/run" },
            { label: "config edit", slug: "reference/commands/config" },
            { label: "init", slug: "reference/commands/init" },
            { label: "setup", slug: "reference/commands/setup" },
            { label: "claude prune", slug: "reference/commands/claude" },
            { label: "sandbox", slug: "reference/commands/sandbox" },
            { label: "remote", slug: "reference/commands/remote" },
            { label: "completions", slug: "reference/commands/completions" },
            { label: "docs", slug: "reference/commands/docs" },
            { label: "changelog", slug: "reference/commands/changelog" },
            { label: "update", slug: "reference/commands/update" },
            { label: "last-done", slug: "reference/commands/last-done" },
            { label: "last-agent", slug: "reference/commands/last-agent" },
          ],
        },
      ],
    }),
  ],
});
