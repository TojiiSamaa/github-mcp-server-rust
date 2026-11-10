#!/usr/bin/env node
// Génère crates/server/tests/data/routes_go.tsv : méthode + route HTTP appelées par chaque outil Go
// (github-mcp-server/pkg/github) pour les outils présents dans le registre Rust.
// Analyse statique (graphe d'appels depuis la fonction qui déclare l'outil) ; route go-github lue dans
// les commentaires `//meta:operation`. Ce qui n'est pas résolu sort en verbe `?` + raison, jamais inventé.
// Env : GO_SERVER (défaut ../github-mcp-server), GO_GITHUB_DIR (défaut <`go env GOMODCACHE`>/github.com/google/go-github/v<N>@<version de go.mod>/github).
// Rejeu (aucune copie à faire, go-github vient du cache de modules Go) :
//   (cd ../github-mcp-server && go mod download) && node scripts/extraire-routes-go.mjs
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const goServer = process.env.GO_SERVER || path.resolve(root, '../github-mcp-server');
const pkgDir = path.join(goServer, 'pkg/github');
const gomod = fs.readFileSync(path.join(goServer, 'go.mod'), 'utf8');
const [, ghMod, ghVer] = gomod.match(/github\.com\/google\/(go-github\/v\d+) (v[\d.]+)/);
const modCache = () => execFileSync('go', ['env', 'GOMODCACHE'], { cwd: goServer, encoding: 'utf8' }).trim();
const ghDir = process.env.GO_GITHUB_DIR || path.join(modCache(), 'github.com/google', `${ghMod}@${ghVer}`, 'github');
if (!fs.existsSync(ghDir)) throw new Error(`go-github introuvable: ${ghDir}
Lance : (cd ${goServer} && go mod download)  — ou définis GO_GITHUB_DIR`);

// --- outils du registre Rust : t!(x, "nom", ..) et Tool::official("nom") ---
const rustTools = new Set();
for (const c of fs.readdirSync(path.join(root, 'crates'))) {
  const d = path.join(root, 'crates', c, 'src');
  if (!fs.existsSync(d)) continue;
  for (const f of fs.readdirSync(d)) {
    if (!f.endsWith('.rs')) continue;
    for (const m of fs.readFileSync(path.join(d, f), 'utf8').matchAll(/(?:\bt!\(\w+,\s*|official\()"([a-z_]+)"/g)) rustTools.add(m[1]);
  }
}
rustTools.delete('nom_du_snap');

// enum `method` d'un outil : lu dans le snap embarqué côté Rust
const snapDir = path.join(root, 'schemas/official');
function snapMethods(tool) {
  for (const f of fs.readdirSync(snapDir)) {
    if (f !== `${tool}.snap` && !f.startsWith(`${tool}_ff_`)) continue;
    const e = JSON.parse(fs.readFileSync(path.join(snapDir, f), 'utf8')).inputSchema?.properties?.method;
    if (e) return e.enum || null;
  }
  return null;
}

// --- go-github : service -> type, (type, méthode) -> opérations ---
const readGo = (p) => fs.readFileSync(p, 'utf8').replace(/\r\n/g, '\n');
const svcType = {}; // champ du client -> type
for (const m of readGo(path.join(ghDir, 'github.go')).matchAll(/^\t(\w+)\s+\*(\w+Service)\b/gm)) svcType[m[1]] = m[2];
const ops = {}; // "IssuesService.Get" -> ["GET /repos/..."]
for (const f of fs.readdirSync(ghDir)) {
  if (!f.endsWith('.go') || f.endsWith('_test.go')) continue;
  const lines = readGo(path.join(ghDir, f)).split('\n');
  let pending = [];
  for (const l of lines) {
    const mo = l.match(/^\/\/meta:operation (\w+) (\S+)/);
    if (mo) { pending.push(`${mo[1]} ${mo[2]}`); continue; }
    const fn = l.match(/^func \(\w+ \*?(\w+Service)\) (\w+)\(/);
    if (fn && pending.length) ops[`${fn[1]}.${fn[2]}`] = pending;
    if (!l.startsWith('//')) pending = [];
  }
}

// --- source Go : fonctions de premier niveau, commentaires/chaînes masqués à offsets constants ---
function mask(src, keepStrings) {
  let out = '', i = 0;
  const n = src.length;
  while (i < n) {
    const c = src[i];
    if (c === '/' && src[i + 1] === '/') { while (i < n && src[i] !== '\n') { out += ' '; i++; } continue; }
    if (c === '/' && src[i + 1] === '*') { while (i < n && !(src[i] === '*' && src[i + 1] === '/')) { out += src[i] === '\n' ? '\n' : ' '; i++; } out += '  '; i += 2; continue; }
    if (c === '"' || c === '`' || c === "'") {
      const q = c; out += q; i++;
      while (i < n && src[i] !== q) {
        if (src[i] === '\\' && q !== '`') { out += keepStrings ? src.slice(i, i + 2) : '  '; i += 2; continue; }
        out += keepStrings || src[i] === '\n' ? src[i] : ' '; i++;
      }
      out += q; i++; continue;
    }
    out += c; i++;
  }
  return out;
}
const files = fs.readdirSync(pkgDir).filter((f) => f.endsWith('.go') && !f.endsWith('_test.go'));
const funcs = {}; // nom -> {file, start, end, src, skel, text}
const fileInfo = {};
for (const f of files) {
  const raw = readGo(path.join(pkgDir, f));
  const src = mask(raw, true), skel = mask(raw, false);
  const nl = []; for (let i = 0; i < raw.length; i++) if (raw[i] === '\n') nl.push(i);
  fileInfo[f] = { src, skel, nl };
  const re = /^func (?:\([^)]*\) )?(\w+)/gm;
  for (let m; (m = re.exec(skel));) {
    const end = skel.indexOf('\n}\n', m.index);
    funcs[m[1]] ??= { file: f, start: m.index, end: end < 0 ? skel.length : end + 2 };
  }
}
const consts = {}; // valeur -> [identifiants de constante]
for (const f of files) for (const m of fileInfo[f].src.matchAll(/^\s*(\w+)\s+=\s+"([^"]+)"\s*$/gm)) (consts[m[2]] ??= []).push(m[1]);
const lineOf = (f, off) => { const nl = fileInfo[f].nl; let lo = 0, hi = nl.length; while (lo < hi) { const m = (lo + hi) >> 1; if (nl[m] < off) lo = m + 1; else hi = m; } return lo + 1; };

// --- fonction qui déclare l'outil ---
function declaringFunc(tool) {
  const pats = [new RegExp(`Name:\\s*"${tool}"`), new RegExp(`\\(t,\\s*"${tool}"`), new RegExp(`^\\s*"${tool}",\\s*$`, 'm')];
  for (const p of pats) for (const f of files) {
    const m = p.exec(fileInfo[f].src);
    if (!m) continue;
    for (const [n, fn] of Object.entries(funcs)) if (fn.file === f && fn.start <= m.index && m.index < fn.end) return n;
  }
  return null;
}

// --- extraction ordonnée des appels API d'un fragment ---
const SKIP = /ifc|lockdown|visibility|cache|telemetry|Flags?\b/i;
function params(name) { // noms des paramètres d'une fonction Go
  const fn = funcs[name], src = fileInfo[fn.file].src;
  const o = src.indexOf('(', src.indexOf(name, fn.start));
  return splitArgs(callArgs(src, o)).map((p) => p.trim().split(/\s+/)[0]).filter(Boolean);
}
function resolveExpr(f, off, expr, bind = {}, fnStart = 0) { // expr de chemin -> route avec {x}
  expr = expr.trim();
  const lit = expr.match(/^"([^"]*)"$/);
  if (lit) return lit[1];
  const sp = expr.match(/^fmt\.Sprintf\(\s*"([^"]*)"/);
  if (sp) {
    // arguments appariés (l'affectation capturée peut déborder sur la ligne suivante) ; url.PathEscape(x) -> x
    const args = splitArgs(callArgs(expr, expr.indexOf('('))).slice(1)
      .map((a) => a.trim().replace(/^url\.PathEscape\(([\s\S]*)\)$/, '$1')).filter(Boolean);
    let i = 0;
    return sp[1].replace(/%[sdvq]/g, () => `{${(args[i++] || 'x').replace(/^.*\./, '').replace(/\W+/g, '') || 'x'}}`);
  }
  if (/^\w+$/.test(expr)) { // variable : affectation dans la fonction, sinon paramètre lié par l'appelant, sinon dernière du fichier
    const local = fileInfo[f].src.slice(fnStart, off);
    const reL = new RegExp(`\\b${expr}\\s*:?=\\s*([^\\n]+(?:\\n\\s+[^\\n]*\\)\\s*)?)`, 'g');
    let lastL; for (let m; (m = reL.exec(local));) lastL = m[1];
    if (lastL) return resolveExpr(f, off, lastL.trim(), bind, fnStart);
    if (bind[expr]) return bind[expr];
    const before = fileInfo[f].src.slice(0, off);
    const re = new RegExp(`\\b${expr}\\s*:?=\\s*([^\\n]+(?:\\n\\s+[^\\n]*\\)\\s*)?)`, 'g');
    let last; for (let m; (m = re.exec(before));) last = m[1];
    if (last) return resolveExpr(f, off, last.trim());
  }
  return null;
}
function callArgs(text, openIdx) { // texte entre ( ... ) apparié
  let d = 0;
  for (let i = openIdx; i < text.length; i++) {
    if (text[i] === '(') d++;
    else if (text[i] === ')' && --d === 0) return text.slice(openIdx + 1, i);
  }
  return '';
}
function splitArgs(s) {
  const out = []; let d = 0, cur = '', q = null;
  for (const c of s) {
    if (q) { cur += c; if (c === q) q = null; continue; }
    if (c === '"' || c === '`') { q = c; cur += c; continue; }
    if ('([{'.includes(c)) d++; else if (')]}'.includes(c)) d--;
    if (c === ',' && d === 0) { out.push(cur); cur = ''; } else cur += c;
  }
  out.push(cur);
  return out;
}
function gqlName(f, off, args) {
  const v = (args[1] || '').trim().replace(/^&/, '');
  if (!/^\w+$/.test(v)) return null;
  const before = fileInfo[f].src.slice(0, off);
  const re = new RegExp(`\\b${v}\\s*(?::=\\s*&?(\\w+)\\{|\\s+(struct)\\s*\\{|\\s+\\*?(\\w+)\\b)`, 'g');
  let last; for (let m; (m = re.exec(before));) last = m;
  if (!last) return null;
  if (last[1] || last[3]) return last[1] || last[3];
  const body = before.slice(last.index + last[0].length);
  const fld = body.match(/\s*(\w+)\s+[\w.*\[\]]*\s*(?:struct\s*\{|`graphql:"([^"]+)"`)/);
  return fld ? (fld[2] || fld[1]) : null;
}

function scan(name, out, seen, depth, mode, bind = {}) {
  // mode: {from,to} sous-intervalle (case) ou null = fonction entière
  const fn = funcs[name];
  if (!fn || SKIP.test(name) || depth > 5 || seen.has(name)) return;
  if (!mode) seen.add(name);
  const { src } = fileInfo[fn.file];
  const from = mode ? mode.from : fn.start, to = mode ? mode.to : fn.end;
  const text = src.slice(0, to);
  const re = /(?<![.\w])(\w+)\(|\.(\w+)\.(\w+)\(|NewRequest\(|\.(Query|Mutate)\(/g;
  re.lastIndex = from;
  for (let m; (m = re.exec(text));) {
    const line = `${fn.file}:${lineOf(fn.file, m.index)}`;
    if (m[3]) { // service.Method(
      const t = svcType[m[2]];
      if (t && ops[`${t}.${m[3]}`]) {
        let list = ops[`${t}.${m[3]}`];
        if (list.length > 1) { // plusieurs routes : seul un 1er argument "" (utilisateur courant) lève le doute
          const a1 = splitArgs(callArgs(text, m.index + m[0].length - 1))[1]?.trim();
          const bare = list.filter((o) => !o.includes('{'));
          list = a1 === '""' && bare.length === 1 ? bare : [];
        }
        if (list.length) for (const o of list) { const [v, r] = o.split(' '); out.push([v, r, line]); }
        else out.push(['?', `${m[2]}.${m[3]}`, `${line} plusieurs routes go-github: ${ops[`${t}.${m[3]}`].join(' | ')}`]);
      }
      else if (t) out.push(['?', `${m[2]}.${m[3]}`, `${line} sans meta:operation`]);
    } else if (m[0] === 'NewRequest(') {
      const a = splitArgs(callArgs(text, m.index + m[0].length - 1));
      const verb = (a[1] || '').trim().match(/^http\.Method(\w+)$/)?.[1]?.toUpperCase() ?? (a[1] || '').trim().match(/^"(\w+)"$/)?.[1];
      const route = resolveExpr(fn.file, m.index, a[2] || '', bind, fn.start);
      if (verb && route) out.push([verb, route, line]);
      else out.push(['?', (a[2] || '').trim(), `${line} NewRequest non résolu`]);
    } else if (m[4]) {
      const a = splitArgs(callArgs(text, m.index + m[0].length - 1));
      const g = gqlName(fn.file, m.index, a);
      out.push(g ? ['GRAPHQL', g, line] : ['?', m[4], `${line} nom GraphQL non résolu`]);
    } else if (funcs[m[1]] && m[1] !== name && !/^(func|if|for|switch|return)$/.test(m[1])) {
      const a = splitArgs(callArgs(text, m.index + m[0].length - 1)), b = {};
      params(m[1]).forEach((p, i) => { const r = a[i] && resolveExpr(fn.file, m.index, a[i], bind, fn.start); if (r && r.includes('/')) b[p] = r; });
      scan(m[1], out, seen, depth + 1, null, b);
    }
  }
}

// --- corps du `case "<m>"` : cherche d'abord dans la fonction de l'outil puis dans ses appelées ---
function findCase(start, method) {
  const order = [], seen = new Set();
  (function walk(n, d) {
    if (!funcs[n] || seen.has(n) || d > 4 || SKIP.test(n)) return;
    seen.add(n); order.push(n);
    const fn = funcs[n];
    for (const m of fileInfo[fn.file].src.slice(fn.start, fn.end).matchAll(/(?<![.\w])(\w+)\(/g)) walk(m[1], d + 1);
  })(start, 0);
  const alts = [JSON.stringify(method), ...(consts[method] || [])];
  const isLabel = (lbl) => lbl.split(',').some((x) => alts.includes(x.trim()));
  const caseRe = /\bcase\s+([^:\n]*):/g;
  const found = [];
  for (const n of order) {
    const fn = funcs[n], { skel, src } = fileInfo[fn.file];
    const sk = skel.slice(fn.start, fn.end), sr = src.slice(fn.start, fn.end);
    for (let top = 0; ;) { // chaque case du niveau trouvé (le 1er peut être un case vide de validation)
      let range = null, from = top;
      for (;;) { // cas imbriqués (case a, b: ... switch { case a: }) : on affine jusqu'au plus profond
        caseRe.lastIndex = from;
        let m, hit = null;
        while ((m = caseRe.exec(sr)) && (!range || m.index < range.to)) if (isLabel(m[1])) { hit = m; break; }
        if (!hit) break;
        let d = 0, i = hit.index + hit[0].length;
        for (; i < sk.length; i++) {
          if (sk[i] === '{') d++; else if (sk[i] === '}') { if (--d < 0) break; }
          else if (d === 0 && /^(case\b|default\s*:)/.test(sk.slice(i, i + 9)) && /\s/.test(sk[i - 1] || ' ')) break;
        }
        if (!range) top = hit.index + hit[0].length;
        range = { from: hit.index, to: i }; from = hit.index + hit[0].length;
      }
      if (!range) break;
      found.push({ name: n, from: fn.start + range.from, to: fn.start + range.to });
    }
  }
  return found;
}

// --- sortie ---
const rows = [];
let covered = 0, unresolved = 0;
const goNames = new Set();
for (const tool of [...rustTools].sort()) {
  const decl = declaringFunc(tool);
  if (!decl) continue;
  goNames.add(tool);
  let methods = snapMethods(tool);
  const toolRows = [];
  if (methods) {
    for (const m of methods) {
      const cs = findCase(decl, m), out = [];
      for (const c of cs) { scan(c.name, out, new Set(), 0, { from: c.from, to: c.to }); if (out.length) break; }
      if (!cs.length) out.push(['?', '', `${funcs[decl].file} case "${m}" introuvable`]);
      toolRows.push(...out.map((o) => [tool, m, ...o]));
    }
  } else {
    const out = []; scan(decl, out, new Set(), 0, null);
    if (!out.length) out.push(['?', '', `${funcs[decl].file} aucun appel API résolu`]);
    toolRows.push(...out.map((o) => [tool, '', ...o]));
  }
  // dédoublonne les appels consécutifs identiques d'un même (outil, méthode)
  for (const r of toolRows) { const p = rows[rows.length - 1]; if (!(p && p[0] === r[0] && p[1] === r[1] && p[2] === r[2] && p[3] === r[3])) rows.push(r); }
}
for (const r of rows) if (r[2] === '?') unresolved++;
covered = new Set(rows.filter((r) => r[2] !== '?').map((r) => r[0])).size;
const outFile = path.join(root, 'crates/server/tests/data/routes_go.tsv');
fs.mkdirSync(path.dirname(outFile), { recursive: true });
fs.writeFileSync(outFile, ['outil\tmethode_outil\tverbe\troute\tsource', ...rows.map((r) => r.join('\t'))].join('\n') + '\n');
console.error(`outils Rust: ${rustTools.size}, déclarés côté Go: ${goNames.size}, avec ≥1 route résolue: ${covered}, lignes: ${rows.length}, lignes '?': ${unresolved}`);
