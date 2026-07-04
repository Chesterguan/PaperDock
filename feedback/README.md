# PaperDock feedback collector

A tiny Cloudflare Worker that tallies 👍/👎 by research field. No domain needed —
it runs on a free `*.workers.dev` subdomain. It stores only a coarse count; the
app never sends papers, questions, or identifiers.

## Deploy (one time, ~3 min)

You need a (free) Cloudflare account and [`wrangler`](https://developers.cloudflare.com/workers/wrangler/install-and-update/)
(`npm i -g wrangler`).

```bash
cd feedback
wrangler login
# create the KV store, then paste the printed id into wrangler.toml's `id = ...`
wrangler kv namespace create PD_FEEDBACK
wrangler deploy
```

`wrangler deploy` prints your URL, e.g. `https://paperdock-feedback.<you>.workers.dev`.

## Wire it up

Give that URL to the app + landing page:

1. **App:** set `FEEDBACK_URL` in `src-tauri/src/lib.rs` to the URL, then rebuild.
   Empty (the default) means telemetry is off and no feedback is ever sent.
2. **Landing page:** the stats section in `docs/index.html` fetches `<URL>/stats`
   client-side (no GitHub Action needed — it's always live). Set the URL there too.

## Check it

```bash
curl -X POST https://paperdock-feedback.<you>.workers.dev \
  -H 'content-type: application/json' \
  -d '{"rating":"up","field":"clinical ml","version":"0.3.0"}'
curl https://paperdock-feedback.<you>.workers.dev/stats
```

## Why no weekly GitHub Action?

We considered a scheduled Action that rewrites the landing page. A live
client-side fetch of `/stats` is simpler and always current — one less moving
part. If you ever want the numbers baked into the committed HTML instead (e.g.
to show them with JS disabled), add an Action that `curl`s `/stats` and commits
the result; the endpoint is the same.
