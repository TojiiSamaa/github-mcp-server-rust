// Génère bench/chart.svg depuis bench/results.json : barres comparées officiel (Go) vs ghmcp (Rust).
// Couleurs = créneaux 1 (bleu) et 2 (orange) de la palette dataviz validée ; clair/sombre via prefers-color-scheme.
import { readFileSync, writeFileSync } from "node:fs";

const r = JSON.parse(readFileSync(process.argv[2] ?? "bench/results.json", "utf8"));
const { go, go_all: all, rust } = r;
const MB = (b) => b / 1048576;
const fmt = (v) => (v >= 100 ? Math.round(v) : +v.toFixed(1)).toString().replace(".", ",");
// [titre, Go défaut, Go all, Rust, unité] ; plus petit = meilleur, sauf « outils » (information, pas un score)
const panels = [
  ["Taille du binaire", MB(go.binary_bytes), MB(all.binary_bytes), MB(rust.binary_bytes), "Mo"],
  ["Mémoire au repos (RSS)", MB(go.rss_idle_bytes), MB(all.rss_idle_bytes), MB(rust.rss_idle_bytes), "Mo"],
  [`Démarrage jusqu'à tools/list (médiane de ${r.trials}, min-max dans le README)`, go.startup_ms_median, all.startup_ms_median, rust.startup_ms_median, "ms"],
  ["Outils exposés", go.tools, all.tools, rust.tools, ""],
];
const W = 720, PAD = 24, LAB = 130, BARW = W - PAD * 2 - LAB - 90, ROW = 22, GAP = 4, PH = 3 * ROW + 2 * GAP + 40;
const H = 96 + panels.length * PH + 40;
const esc = (t) => String(t).replace(/&/g, "&amp;").replace(/</g, "&lt;");
let y = 96, body = "";
for (const [title, a, a2, b, unit] of panels) {
  const max = Math.max(a, a2, b) || 1;
  body += `<text x="${PAD}" y="${y + 12}" class="t2">${esc(title)}</text>`;
  [[a, "s1", "Go, défaut"], [a2, "s3", "Go, --toolsets all"], [b, "s2", "ghmcp (Rust)"]].forEach(([v, c, name], i) => {
    const by = y + 22 + i * (ROW + GAP), w = Math.max(3, (v / max) * BARW);
    body += `<text x="${PAD + LAB - 8}" y="${by + 15}" text-anchor="end" class="t2 sm">${name}</text>`;
    body += `<path d="M${PAD + LAB},${by}H${PAD + LAB + w - 4}a4,4 0 0 1 4,4V${by + ROW - 4}a4,4 0 0 1 -4,4H${PAD + LAB}Z" class="${c}"/>`;
    body += `<text x="${PAD + LAB + w + 8}" y="${by + 15}" class="t1">${fmt(v)}${unit ? " " + unit : ""}</text>`;
  });
  y += PH;
  body += `<line x1="${PAD}" x2="${W - PAD}" y1="${y - 14}" y2="${y - 14}" class="grid"/>`;
}
const foot = [`${r.platform}`, `Go ${r.commits.go.slice(0, 7)} (${r.go_version.replace("go version ", "")}) · ghmcp ${r.commits.rust.slice(0, 7)} (${r.rustc_version.split(" (")[0]}) · ${r.trials} essais · ${r.generated.slice(0, 10)}`];
const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="${W}" height="${H}" viewBox="0 0 ${W} ${H}" role="img" aria-label="Banc github-mcp-server Go contre ghmcp Rust">
<style>
svg{--bg:#fcfcfb;--t1:#0b0b0b;--t2:#52514e;--mu:#898781;--grid:#e1e0d9;--s1:#2a78d6;--s2:#eb6834;--s3:#7a5cc7}
@media (prefers-color-scheme:dark){svg{--bg:#1a1a19;--t1:#f0efec;--t2:#c3c2b7;--mu:#898781;--grid:#2c2c2a;--s1:#3987e5;--s2:#d95926;--s3:#9b82e0}}
text{font-family:system-ui,-apple-system,"Segoe UI",sans-serif}
.bg{fill:var(--bg)}.t1{fill:var(--t1);font-size:13px;font-weight:600}.t2{fill:var(--t2);font-size:13px}.sm{font-size:12px}.mu{fill:var(--mu);font-size:11px}
.h{fill:var(--t1);font-size:18px;font-weight:600}.grid{stroke:var(--grid);stroke-width:1}.s1{fill:var(--s1)}.s2{fill:var(--s2)}.s3{fill:var(--s3)}
</style>
<rect class="bg" width="${W}" height="${H}"/>
<text x="${PAD}" y="34" class="h">github-mcp-server (Go) contre ghmcp (Rust)</text>
<text x="${PAD}" y="54" class="t2">Sans réseau, jeton factice. Plus court = mieux, sauf le nombre d'outils.</text>
<rect x="${PAD}" y="66" width="10" height="10" rx="2" class="s1"/><text x="${PAD + 16}" y="75" class="t2 sm">Officiel (Go), toolsets par défaut</text>
<rect x="${PAD + 230}" y="66" width="10" height="10" rx="2" class="s3"/><text x="${PAD + 246}" y="75" class="t2 sm">Officiel (Go), --toolsets all</text>
<rect x="${PAD + 440}" y="66" width="10" height="10" rx="2" class="s2"/><text x="${PAD + 456}" y="75" class="t2 sm">ghmcp (Rust)</text>
${body}<text x="${PAD}" y="${H - 24}" class="mu">${esc(foot[0])}</text><text x="${PAD}" y="${H - 8}" class="mu">${esc(foot[1])}</text>
</svg>
`;
writeFileSync(process.argv[3] ?? "bench/chart.svg", svg);
