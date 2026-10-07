#!/usr/bin/env node
import { createHash } from "node:crypto";
import { readFileSync, readdirSync, statSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import ts from "typescript";

const root = resolve(new URL("..", import.meta.url).pathname);
const sourceRoot = join(root, "src");
const localeDir = join(sourceRoot, "locales");
const englishPath = join(localeDir, "en-US.json");
const portuguesePath = join(localeDir, "pt-BR.json");
const english = JSON.parse(readFileSync(englishPath, "utf8"));
const portuguese = JSON.parse(readFileSync(portuguesePath, "utf8"));
const attributes = new Set([
  "title", "placeholder", "aria-label", "alt", "data-text", "loadingText",
  "label", "description", "topic",
]);

function files(directory) {
  return readdirSync(directory).flatMap((name) => {
    const path = join(directory, name);
    return statSync(path).isDirectory() ? files(path) : path.endsWith(".tsx") ? [path] : [];
  });
}

function keyFor(text) {
  return `generated.${createHash("sha256").update(text).digest("hex").slice(0, 16)}`;
}

for (const path of files(sourceRoot)) {
  if (path.endsWith("i18n.tsx")) continue;
  let source = readFileSync(path, "utf8");
  const file = ts.createSourceFile(path, source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  const edits = [];
  function addText(text) {
    const normalized = text.replace(/\s+/g, " ").trim();
    if (!/[A-Za-zÀ-ÿ]/u.test(normalized)) return null;
    const key = keyFor(normalized);
    english[key] ??= normalized;
    portuguese[key] ??= normalized;
    return key;
  }
  function visit(node) {
    if (ts.isJsxText(node)) {
      const key = addText(node.getText(file));
      if (key) {
        const raw = node.getText(file);
        const leading = raw.match(/^\s*/u)?.[0] ?? "";
        const trailing = raw.match(/\s*$/u)?.[0] ?? "";
        edits.push([node.getStart(file), node.getEnd(), `${leading}{translate("${key}")}${trailing}`]);
      }
    } else if (ts.isJsxAttribute(node) && attributes.has(node.name.getText(file)) && node.initializer && ts.isStringLiteral(node.initializer)) {
      const key = addText(node.initializer.text);
      if (key) edits.push([node.initializer.getStart(file), node.initializer.getEnd(), `{translate("${key}")}`]);
    } else if (ts.isStringLiteral(node) && ts.isConditionalExpression(node.parent)
      && (node.parent.whenTrue === node || node.parent.whenFalse === node)) {
      let parent = node.parent;
      while (parent && !ts.isJsxExpression(parent) && !ts.isJsxAttribute(parent)) parent = parent.parent;
      const attribute = parent && ts.isJsxExpression(parent) && ts.isJsxAttribute(parent.parent)
        ? parent.parent
        : parent && ts.isJsxAttribute(parent) ? parent : null;
      if (parent && (!attribute || attributes.has(attribute.name.getText(file)))) {
        const key = addText(node.text);
        if (key) edits.push([node.getStart(file), node.getEnd(), `translate("${key}")`]);
      }
    }
    ts.forEachChild(node, visit);
  }
  visit(file);
  if (!edits.length) continue;
  for (const [start, end, replacement] of edits.sort((a, b) => b[0] - a[0])) {
    source = source.slice(0, start) + replacement + source.slice(end);
  }
  if (!source.includes('from "../../lib/i18n"') && !source.includes('from "../lib/i18n"')) {
    const relative = path.includes(`${join("src", "components")}`) || path.includes(`${join("src", "features")}`)
      ? "../../lib/i18n"
      : "../lib/i18n";
    source = `import { translate } from "${relative}";\n${source}`;
  } else {
    source = source.replace(/import \{ ([^}]+) \} from "(\.\.\/\.\.\/lib\/i18n|\.\.\/lib\/i18n)";/u, (all, names, module) =>
      names.includes("translate") ? all : `import { ${names}, translate } from "${module}";`);
  }
  writeFileSync(path, source);
}

writeFileSync(englishPath, `${JSON.stringify(english, null, 2)}\n`);
writeFileSync(portuguesePath, `${JSON.stringify(portuguese, null, 2)}\n`);
