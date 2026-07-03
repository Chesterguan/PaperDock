# PaperDock

Ask questions about your Zotero papers and get answers with citations you can
click to open the source PDF. Runs on your Mac; your library never leaves it.

---

## 1. Install

1. Download **`PaperDock_0.1.0_universal.dmg`** (works on both Apple Silicon and
   Intel Macs).
2. Open the DMG, drag **PaperDock** into Applications.
3. **First launch: right-click PaperDock → Open** (then click *Open* in the
   dialog). The normal double-click is blocked because the app isn't
   code-signed — you only need the right-click trick the first time.
4. On first launch PaperDock does a **one-time setup** (downloads a small Python
   environment, ~1–2 min, needs internet). Click **Set up now** and wait for it
   to finish. This happens once.

---

## 2. Set up Zotero (do this once) — important

PaperDock reads your library through Zotero's local connection. Three things
must be true, and **step 2 is the one people miss:**

1. **Use Zotero 7 or newer.** The local connection PaperDock needs does not
   exist in Zotero 6. (Check *Zotero → About Zotero*.)

2. **Turn on the local connection.** In Zotero:
   **Settings → Advanced → General →** check
   **"Allow other applications on this computer to communicate with Zotero."**
   Without this, PaperDock stays stuck on *"Waiting for Zotero…"* no matter what.
   *(You do **not** need to log in to a Zotero account — PaperDock only reads
   your local library.)*

3. **Keep Zotero running** while you use PaperDock, and make sure the papers you
   want to ask about are **in a collection** with their **PDFs downloaded**
   (a paper with no PDF can't be read).

**Quick check:** with Zotero open, visit this in any browser —
`http://localhost:23119/api/users/0/items/top?limit=1`.
If it returns something (even empty), the connection is on.

---

## 3. Use it

1. Pick a **collection** from the dropdown.
2. Type a question and press **Enter**.
3. You get a grounded answer with **citations** — click one to open that paper
   in Zotero. Ask follow-up questions in the same thread.

---

## Setting up your keys (v0.2+)

PaperDock ships without any keys. Your lab's **admin** configures the backend
once and shares a small `.paperdock` file; everyone else just opens it.

### Get a key — NaviGator (UF) walkthrough

If your lab uses **UF NaviGator** (the example used throughout this guide), each
person can get their own key in ~2 minutes:

1. Go to **https://api.ai.it.ufl.edu/ui** and sign in with your **GatorLink**.
2. Click **Create New Key**. Pick the team (default `navigator-toolkit`), name the
   key (e.g. `PaperDock`), and select the models you'll use — at least a chat model
   (`gpt-oss-120b`) and an embedding model (`nomic-embed-text-v1.5`). Click
   **Create Key**.
3. **Copy the key now** — it's shown only once. Every UF student/faculty/staff gets
   **$100/month** of credit for personal keys (resets monthly).
4. In PaperDock → **⚙ Settings**, enter:
   - **API base:** `https://api.ai.it.ufl.edu/v1`
   - **Model:** `openai/gpt-oss-120b`  ·  **Embedding:** `openai/nomic-embed-text-v1.5`
     (the `openai/` prefix tells PaperDock to talk to NaviGator's OpenAI-compatible API)
   - **LLM key:** the key you copied

   Click **Save**. Done — that's the whole setup.

> Need a **shared team budget** or cloud models beyond the personal $100/month? A
> lab admin can request a team via the **UFIT Help Desk Portal**. Current model
> list: <https://docs.ai.it.ufl.edu/docs/navigator_models/>.

**Not at UF?** Use any provider with the same three fields — OpenAI (key from
<https://platform.openai.com>, base blank, model `gpt-4o`) or a local **Ollama**
server (no key, base `http://localhost:11434`, model `ollama/llama3.1`).

### Admin — set up your lab once
1. Open PaperDock → **⚙ Settings** and fill in:
   - **Model / Embedding / API base / LLM key** for your provider. Examples:
     - **UF NaviGator:** base `https://api.ai.it.ufl.edu/v1`, model
       `openai/gpt-oss-120b`, embedding `openai/nomic-embed-text-v1.5`, plus your
       NaviGator key.
     - **OpenAI:** base blank, model `gpt-4o`, embedding
       `text-embedding-3-small`, your OpenAI key.
     - **Local Ollama:** base `http://localhost:11434`, model
       `ollama/llama3.1`, embedding `ollama/nomic-embed-text`, no key needed.
   - **Qdrant URL + key** (optional) for a shared team vector store — a free
     [Qdrant Cloud](https://qdrant.tech/) cluster works. Leave blank to keep
     every member's embeddings local.
2. Click **Export lab config…**. Leave **"Include LLM key"** checked to give
   members a zero-setup file; uncheck it if members use their own keys.
3. Share the resulting `.paperdock` file **privately** (email/Slack/OneDrive to
   named people) — it contains your keys.

### Member — join a lab
1. Install PaperDock (see Install above).
2. **Double-click the `.paperdock` file** your admin sent — the app configures
   itself and shows "Lab config imported ✓". (Or use **Import lab config…** on
   the first-run screen / in Settings.)
3. If your lab uses personal keys, get your own key (see **Get a key** above) and
   paste it in **⚙ Settings**.

### Using a shared team library
The shared vector store is scoped to a **Zotero group library**. To share
embeddings with your lab, join that group in Zotero (Zotero → your account →
Groups). Personal-library papers always embed **locally** on your own machine.

> **Security:** the `.paperdock` file and any API keys are secrets. Anyone with
> them can use your LLM quota and read/modify your shared vector store. Share the
> file only with people you trust; don't post it publicly or commit it to git.

---

## Troubleshooting

| You see… | Fix |
|---|---|
| **"Waiting for Zotero…"** (won't clear) | Zotero isn't running, isn't v7+, or the **"Allow other applications…"** setting (step 2 above) is off. |
| **"no Zotero collections found"** | Your library/collection is empty — add papers to a collection in Zotero. |
| **"…papers have no PDF downloaded"** | Open those papers in Zotero and download/sync their PDFs, then ask again. |
| macOS says the app **"is damaged" / can't be opened** | Right-click the app → **Open** (it's unsigned, not actually damaged). |
| Setup screen shows an **error** | Check your internet connection and click **Try again** — first-run setup needs to download packages. |
