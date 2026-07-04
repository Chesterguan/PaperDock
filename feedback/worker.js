// PaperDock feedback collector — a Cloudflare Worker (free *.workers.dev).
//
// POST /            {rating:"up"|"down", field, version}  → increments a tally
// GET  /stats                                             → aggregate JSON
//
// It stores ONLY a coarse tally — a count of 👍/👎 per research field. No paper
// content, no questions, no identifiers. That's all the app ever sends.
//
// ponytail: KV read-modify-write is not atomic — fine at this volume; if
// concurrent writes ever collide meaningfully, move the tally to a Durable Object.
export default {
  async fetch(req, env) {
    const cors = {
      "Access-Control-Allow-Origin": "*",
      "Access-Control-Allow-Methods": "GET,POST,OPTIONS",
      "Access-Control-Allow-Headers": "Content-Type",
    };
    if (req.method === "OPTIONS") return new Response(null, { headers: cors });
    const url = new URL(req.url);

    const empty = { total: 0, up: 0, down: 0, byField: {} };
    const load = async () => {
      const raw = await env.PD_FEEDBACK.get("tally");
      return raw ? JSON.parse(raw) : { ...empty };
    };

    if (req.method === "GET" && url.pathname === "/stats") {
      return Response.json(await load(), { headers: cors });
    }
    if (req.method === "POST") {
      let b;
      try { b = await req.json(); } catch { return new Response("bad json", { status: 400, headers: cors }); }
      const rating = b.rating === "up" ? "up" : b.rating === "down" ? "down" : null;
      if (!rating) return new Response("bad rating", { status: 400, headers: cors });
      const field =
        typeof b.field === "string" && b.field.trim()
          ? b.field.trim().slice(0, 60).toLowerCase()
          : "unspecified";
      const t = await load();
      t.total++;
      t[rating]++;
      t.byField[field] = t.byField[field] || { up: 0, down: 0 };
      t.byField[field][rating]++;
      await env.PD_FEEDBACK.put("tally", JSON.stringify(t));
      return new Response("ok", { headers: cors });
    }
    return new Response("PaperDock feedback collector", { headers: cors });
  },
};
