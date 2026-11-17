const OPENPANEL_TRACK = "https://api.openpanel.dev/track";
/** The site's own project (same as src/analytics.ts); pinned so the relay can't be used for other projects. */
const OPENPANEL_CLIENT_ID = "76801dc5-e15f-47e8-a598-9f73335f96a2";

/**
 * Vercel function: first-party relay for the browser's OpenPanel events (src/analytics.ts).
 * Passes on the visitor's address and agent, since OpenPanel would otherwise see ours.
 */
export async function POST(request: Request) {
  const headers = new Headers({
    "content-type": "application/json",
    "openpanel-client-id": OPENPANEL_CLIENT_ID,
    "openpanel-sdk-name": request.headers.get("openpanel-sdk-name") ?? "",
    "openpanel-sdk-version": request.headers.get("openpanel-sdk-version") ?? "",
    origin: request.headers.get("origin") ?? new URL(request.url).origin,
    "user-agent": request.headers.get("user-agent") ?? "",
  });
  const ip = request.headers.get("x-forwarded-for")?.split(",")[0]?.trim() || request.headers.get("x-real-ip");
  if (ip) headers.set("openpanel-client-ip", ip);

  const upstream = await fetch(OPENPANEL_TRACK, { method: "POST", headers, body: await request.text() });
  return new Response(upstream.body, {
    status: upstream.status,
    headers: { "content-type": upstream.headers.get("content-type") ?? "application/json" },
  });
}
