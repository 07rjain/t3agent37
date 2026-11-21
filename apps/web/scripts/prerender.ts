// After `vite build`: fills dist/index.html with the prerendered site, writes its markdown twin
// (index.md) plus llms.txt and llms-full.txt for agents, and updates vercel.json.
import { htmlToMarkdown, llmsFullTxt, llmsTxt, vercelRoutes } from "@usenotra/agentside/build";
import { rm } from "node:fs/promises";
import { join } from "node:path";

const SITE = "https://t3agent37.subdomain.wtf";
const TITLE = "t3agent37";
const DESCRIPTION =
  "Run your T3 Code agents on pay-as-you-go Agent37 containers and VMs. Edge-authenticated localhost proxy, CRIU checkpoint sleep, live Web UI, and MCP server.";

const web = join(import.meta.dir, "..");
const dist = join(web, "dist");
const ssr = join(web, "dist-ssr");

const { render } = await import(join(ssr, "prerender.js"));
await rm(ssr, { recursive: true });
const rendered = render().replaceAll('="/assets/', '="./assets/');
const root = `<div id="root">${rendered}</div>`;

const index = Bun.file(join(dist, "index.html"));
const html = await index.text();
if (!html.includes('<div id="root"></div>')) throw new Error('dist/index.html has no empty <div id="root">');
await Bun.write(index, html.replace('<div id="root"></div>', root));

const pages = [{ path: "/", title: TITLE, description: DESCRIPTION, markdown: htmlToMarkdown(root, { selector: "#root", siteUrl: SITE }) }];
await Bun.write(join(dist, "index.md"), pages[0].markdown);
await Bun.write(join(dist, "llms.txt"), llmsTxt(pages, { title: TITLE, description: DESCRIPTION, siteUrl: SITE }));
await Bun.write(join(dist, "llms-full.txt"), llmsFullTxt(pages, { siteUrl: SITE }));
await Bun.write(join(web, "vercel.json"), `${JSON.stringify(vercelRoutes(pages.map((page) => page.path)), null, 2)}\n`);
