// Build-time entry: the site's markup, written into index.html so the page has content before
// its script runs, and so agentside can turn it into the page's markdown twin (scripts/prerender.ts).
import { StrictMode } from "react";
import { renderToString } from "react-dom/server";
import { Site } from "./Site";

export const render = () =>
  renderToString(
    <StrictMode>
      <Site />
    </StrictMode>,
  );
