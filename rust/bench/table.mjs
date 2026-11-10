// Régénère les lignes de banc du bloc stats de README.md depuis bench/results.json.
// Les lignes fonctionnalités du bloc sont conservées. Usage : node bench/table.mjs
import { readFileSync, writeFileSync } from "node:fs";
const dir = new URL("../", import.meta.url);
const r = JSON.parse(readFileSync(new URL("bench/results.json", dir), "utf8"));
const mb = (b) => `${(b / 1e6).toFixed(1)} MB`;
const ms = (v) => `${v.toFixed(1)} ms`;
const { go, go_all: all, rust } = r;
const rng = (x) => `${x.startup_ms_min.toFixed(0)}-${x.startup_ms_max.toFixed(0)} ms`;
// Cellule Go = « toolsets par défaut / --toolsets all ».
const bench = [
  `| Startup time (median, to first \`tools/list\`) | ${ms(go.startup_ms_median)} / ${ms(all.startup_ms_median)} | ${ms(rust.startup_ms_median)} |`,
  `| Startup range (min-max, ${r.trials} trials) | ${rng(go)} / ${rng(all)} | ${rng(rust)} |`,
  `| Memory at idle (RSS) | ${mb(go.rss_idle_bytes)} / ${mb(all.rss_idle_bytes)} | ${mb(rust.rss_idle_bytes)} |`,
  `| Binary size | ${mb(go.binary_bytes)} | ${mb(rust.binary_bytes)} |`,
  `| Tools in \`tools/list\` | ${go.tools} / ${all.tools} | ${rust.tools} |`,
  `| Report (${r.trials} trials, ${r.platform}; Go commit \`${r.commits.go.slice(0, 7)}\`, ghmcp commit \`${r.commits.rust.slice(0, 7)}\`) | [bench/results.json](bench/results.json) | [bench/chart.svg](bench/chart.svg) |`,
];
const p = new URL("README.md", dir);
const txt = readFileSync(p, "utf8");
const [b, e] = ["<!-- stats:debut -->", "<!-- stats:fin -->"].map((m) => txt.indexOf(m));
const lines = txt.slice(b, e).split("\n");
const isBench = (l) => /^\| (Startup time|Startup range|Memory at idle|`tools\/list` latency|Binary|Tools in|Report)/.test(l);
const at = lines.findIndex(isBench);
lines[lines.findIndex((l) => l.startsWith("| | Official"))] = "| | Official server (Go): default toolsets / `--toolsets all` | `ghmcp` (Rust) |";
const out = [...lines.slice(0, at), ...bench, ...lines.slice(at).filter((l) => !isBench(l))];
writeFileSync(p, txt.slice(0, b) + out.join("\n") + txt.slice(e));
