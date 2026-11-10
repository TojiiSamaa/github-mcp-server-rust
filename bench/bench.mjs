// Mesure sans réseau : taille, RSS après initialize, démarrage -> réponse tools/list, nombre d'outils.
import { spawn, execFileSync } from "node:child_process";
import { statSync, writeFileSync } from "node:fs";
import os from "node:os";

const E = process.env, TRIALS = +(E.TRIALS || 15);
const FAKE = { ...E, GITHUB_PERSONAL_ACCESS_TOKEN: "ghp_factice" };
const med = (a) => { const s = [...a].sort((x, y) => x - y), m = s.length >> 1; return s.length % 2 ? s[m] : (s[m - 1] + s[m]) / 2; };

function rss(pid) {
  if (process.platform === "win32") {
    const o = execFileSync("tasklist", ["/FI", `PID eq ${pid}`, "/FO", "CSV", "/NH"], { encoding: "latin1" });
    return parseInt(o.split('","')[4].replace(/\D/g, ""), 10) * 1024; // « 12 345 Ko »
  }
  return parseInt(execFileSync("ps", ["-o", "rss=", "-p", String(pid)], { encoding: "utf8" }), 10) * 1024;
}

function trial(bin, args, withRss) {
  return new Promise((resolve, reject) => {
    const t0 = performance.now();
    const p = spawn(bin, args, { env: FAKE, stdio: ["pipe", "pipe", "ignore"] });
    const timer = setTimeout(() => { p.kill(); reject(new Error("timeout " + bin)); }, 20000);
    let buf = "", rssB = 0;
    const send = (m) => p.stdin.write(JSON.stringify({ jsonrpc: "2.0", ...m }) + "\n");
    p.on("error", reject);
    p.stdout.on("data", (d) => {
      buf += d;
      let i;
      while ((i = buf.indexOf("\n")) >= 0) {
        const line = buf.slice(0, i).trim(); buf = buf.slice(i + 1);
        if (!line) continue;
        const r = JSON.parse(line);
        if (r.id === 1) {
          if (withRss) try { rssB = rss(p.pid); } catch (e) { return reject(e); } // hors chrono : tasklist/ps coûte ~1 s
          send({ method: "notifications/initialized" });
          send({ id: 2, method: "tools/list", params: {} });
        } else if (r.id === 2) {
          const ms = performance.now() - t0;
          clearTimeout(timer); p.kill();
          resolve({ ms, rssB, tools: r.result.tools.length });
        }
      }
    });
    send({ id: 1, method: "initialize", params: { protocolVersion: "2025-03-26", capabilities: {}, clientInfo: { name: "bench", version: "1" } } });
  });
}

async function measure(label, bin, args) {
  await trial(bin, args); // échauffement (cache disque), non compté
  const rs = [], mem = [];
  for (let i = 0; i < TRIALS; i++) rs.push(await trial(bin, args, false));
  for (let i = 0; i < TRIALS; i++) mem.push(await trial(bin, args, true));
  return {
    label, binary_bytes: statSync(bin).size, rss_idle_bytes: med(mem.map((r) => r.rssB)),
    startup_ms_median: +med(rs.map((r) => r.ms)).toFixed(1),
    startup_ms_min: +Math.min(...rs.map((r) => r.ms)).toFixed(1), startup_ms_max: +Math.max(...rs.map((r) => r.ms)).toFixed(1),
    tools: rs[0].tools,
  };
}

const go = await measure("Officiel (Go, toolsets par défaut)", E.GO_BIN, ["stdio"]);
const go_all = await measure("Officiel (Go, --toolsets all)", E.GO_BIN, ["stdio", "--toolsets", "all"]);
const rust = await measure("ghmcp (Rust)", E.RS_BIN, ["--config", E.RS_CONFIG, "--test"]);
const out = {
  generated: new Date().toISOString(), os: `${os.type()} ${os.release()} ${process.arch}`, trials: TRIALS, node: process.version,
  go_version: E.GO_VERSION, rustc_version: E.RUSTC_VERSION,
  platform: `${os.type()} ${os.release()} ${process.arch}${E.CPUS ? `, conteneur Docker limité à ${E.CPUS} coeurs` : ""}`,
  commits: { go: E.GO_COMMIT, rust: E.RS_COMMIT },
  method: "spawn -> initialize -> RSS -> notifications/initialized -> tools/list ; sans réseau, jeton factice ; médiane de N essais chronométrés (sans mesure RSS) puis N essais pour le RSS, après 1 échauffement",
  go, go_all, rust,
};
writeFileSync(process.argv[2], JSON.stringify(out, null, 2) + "\n");
console.log(JSON.stringify(out, null, 2));
