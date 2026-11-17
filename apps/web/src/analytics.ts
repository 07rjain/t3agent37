import { OpenPanel } from "@openpanel/web";

/**
 * OpenPanel page views and outgoing links, sent through our own `/api/op` (api/op/track.ts)
 * so blockers that know the vendor's host leave them alone. Nothing is tracked under `vite dev`.
 */
const op = new OpenPanel({
  apiUrl: "/api/op",
  clientId: "76801dc5-e15f-47e8-a598-9f73335f96a2",
  trackScreenViews: true,
  trackOutgoingLinks: true,
  disabled: import.meta.env.DEV,
});

export const track = (name: string, properties?: Record<string, unknown>) => op.track(name, properties);
